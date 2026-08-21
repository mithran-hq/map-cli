#!/usr/bin/env python3
"""Publish an exact immutable component release, resuming equivalent drafts."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
import os
from pathlib import Path
import subprocess
from typing import Protocol
from urllib.error import URLError
from urllib.parse import urlencode, urlsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener


class PublicationError(RuntimeError):
    """The remote release cannot safely converge to the intended release."""


@dataclass(frozen=True)
class Snapshot:
    etag: str
    release: dict[str, object]


class UploadOpener(Protocol):
    def open(self, request: Request): ...


class RefuseUploadRedirects(HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, new_url):
        raise PublicationError("release asset upload refused an HTTP redirect")


class ReleaseClient(Protocol):
    def create(self, metadata: dict[str, object]) -> int | None: ...

    def find_release_ids(self, tag: str) -> list[int]: ...

    def snapshot(self, release_id: int) -> Snapshot: ...

    def download_asset(self, asset_id: int) -> bytes: ...

    def upload_asset(self, release_id: int, upload_url: str, path: Path) -> None: ...

    def publish(self, release_id: int) -> None: ...


class GhReleaseClient:
    def __init__(
        self,
        endpoint: str = "repos/{owner}/{repo}/releases",
        upload_opener: UploadOpener | None = None,
    ) -> None:
        self.endpoint = endpoint
        self.upload_opener = upload_opener or build_opener(RefuseUploadRedirects())

    @staticmethod
    def _run(arguments: list[str], *, check: bool = True) -> subprocess.CompletedProcess[bytes]:
        return subprocess.run(
            ["gh", *arguments],
            check=check,
            stdout=subprocess.PIPE,
            stderr=None,
        )

    def create(self, metadata: dict[str, object]) -> int | None:
        result = self._run(
            [
                "api",
                "--method",
                "POST",
                self.endpoint,
                "-f",
                f"tag_name={metadata['tag_name']}",
                "-F",
                f"target_commitish={metadata['target_commitish']}",
                "-f",
                f"name={metadata['name']}",
                "-f",
                f"body={metadata['body']}",
                "-F",
                "draft=true",
                "-F",
                "prerelease=true",
            ],
            check=False,
        )
        if result.returncode != 0:
            return None
        try:
            release_id = json.loads(result.stdout)["id"]
        except (json.JSONDecodeError, KeyError, TypeError) as error:
            raise PublicationError("release creation returned no numeric id") from error
        if not isinstance(release_id, int):
            raise PublicationError("release creation returned no numeric id")
        return release_id

    def find_release_ids(self, tag: str) -> list[int]:
        result = self._run(
            [
                "api",
                "--method",
                "GET",
                "--paginate",
                "--slurp",
                "-F",
                "per_page=100",
                self.endpoint,
            ]
        )
        try:
            pages = json.loads(result.stdout)
        except json.JSONDecodeError as error:
            raise PublicationError("release discovery returned invalid JSON") from error
        if not isinstance(pages, list):
            raise PublicationError("release discovery returned a non-list")

        release_ids: list[int] = []
        for page in pages:
            if not isinstance(page, list):
                raise PublicationError("release discovery returned a malformed page")
            for release in page:
                if not isinstance(release, dict):
                    raise PublicationError("release discovery returned a malformed release")
                release_tag = release.get("tag_name")
                release_id = release.get("id")
                if (
                    not isinstance(release_tag, str)
                    or not isinstance(release_id, int)
                    or isinstance(release_id, bool)
                ):
                    raise PublicationError("release discovery omitted a tag or numeric id")
                if release_tag == tag:
                    release_ids.append(release_id)
        return release_ids

    def snapshot(self, release_id: int) -> Snapshot:
        result = self._run(["api", "--include", f"{self.endpoint}/{release_id}"])
        normalized = result.stdout.replace(b"\r\n", b"\n")
        try:
            header_bytes, body = normalized.split(b"\n\n", 1)
        except ValueError as error:
            raise PublicationError("release snapshot omitted HTTP headers") from error
        headers: dict[str, str] = {}
        for line in header_bytes.decode("utf-8").splitlines()[1:]:
            if ":" in line:
                key, value = line.split(":", 1)
                headers[key.lower()] = value.strip()
        etag = headers.get("etag", "")
        if not etag:
            raise PublicationError("release snapshot omitted ETag")
        try:
            release = json.loads(body)
        except json.JSONDecodeError as error:
            raise PublicationError("release snapshot returned invalid JSON") from error
        if not isinstance(release, dict):
            raise PublicationError("release snapshot returned a non-object")
        return Snapshot(etag=etag, release=release)

    def download_asset(self, asset_id: int) -> bytes:
        result = self._run(
            [
                "api",
                "-H",
                "Accept: application/octet-stream",
                f"{self.endpoint}/assets/{asset_id}",
            ]
        )
        return result.stdout

    def upload_asset(self, release_id: int, upload_url: str, path: Path) -> None:
        token = os.environ.get("GH_TOKEN")
        if not token:
            raise PublicationError("release asset upload requires GH_TOKEN")
        endpoint = validated_upload_endpoint(upload_url, release_id)
        destination = f"{endpoint}?{urlencode({'name': path.name})}"
        request = Request(
            destination,
            data=path.read_bytes(),
            headers={
                "Accept": "application/vnd.github+json",
                "Authorization": f"Bearer {token}",
                "Content-Type": "application/octet-stream",
                "X-GitHub-Api-Version": "2022-11-28",
            },
            method="POST",
        )
        try:
            with self.upload_opener.open(request) as response:
                response.read()
        except URLError as error:
            raise PublicationError(
                f"release asset upload failed for release {release_id}"
            ) from error

    def publish(self, release_id: int) -> None:
        self._run(
            [
                "api",
                "--silent",
                "--method",
                "PATCH",
                "-F",
                "draft=false",
                f"{self.endpoint}/{release_id}",
            ]
        )


def intended_assets(paths: list[Path]) -> dict[str, Path]:
    assets: dict[str, Path] = {}
    for path in paths:
        if not path.is_file():
            raise PublicationError(f"intended release asset is not a file: {path}")
        if path.name in assets:
            raise PublicationError(f"duplicate intended release asset name: {path.name}")
        assets[path.name] = path
    if not assets:
        raise PublicationError("component release has no intended assets")
    return assets


def validated_upload_endpoint(upload_url: object, release_id: int) -> str:
    if not isinstance(release_id, int) or isinstance(release_id, bool):
        raise PublicationError("release upload requires a numeric release id")
    if not isinstance(upload_url, str):
        raise PublicationError("release snapshot omitted its upload URL")

    template = "{?name,label}"
    if not upload_url.endswith(template):
        raise PublicationError("release upload URL omitted its name template")
    endpoint = upload_url[: -len(template)]
    parsed = urlsplit(endpoint)
    try:
        port = parsed.port
    except ValueError as error:
        raise PublicationError("release upload URL has an invalid port") from error
    if parsed.scheme != "https":
        raise PublicationError("release upload URL must use HTTPS")
    if parsed.hostname != "uploads.github.com":
        raise PublicationError("release upload URL host is not uploads.github.com")
    if parsed.username is not None or parsed.password is not None or port is not None:
        raise PublicationError("release upload URL has unexpected authority fields")
    if parsed.query or parsed.fragment:
        raise PublicationError("release upload URL has an unexpected query or fragment")

    path_parts = parsed.path.split("/")
    if (
        len(path_parts) != 7
        or path_parts[0] != ""
        or path_parts[1] != "repos"
        or not path_parts[2]
        or not path_parts[3]
        or path_parts[4:] != ["releases", str(release_id), "assets"]
    ):
        raise PublicationError("release upload URL is not bound to the selected release id")
    return endpoint


def validate_snapshot(
    client: ReleaseClient,
    snapshot: Snapshot,
    expected: dict[str, object],
    assets: dict[str, Path],
    *,
    allow_missing: bool,
) -> list[str]:
    release = snapshot.release
    for field, expected_value in expected.items():
        if release.get(field) != expected_value:
            raise PublicationError(f"release {field} does not match intended metadata")

    remote_assets = release.get("assets")
    if not isinstance(remote_assets, list):
        raise PublicationError("release snapshot omitted its asset inventory")
    by_name: dict[str, dict[str, object]] = {}
    for item in remote_assets:
        if not isinstance(item, dict) or not isinstance(item.get("name"), str):
            raise PublicationError("release asset inventory is malformed")
        name = item["name"]
        if name in by_name:
            raise PublicationError(f"release contains duplicate asset name: {name}")
        by_name[name] = item

    extras = sorted(set(by_name) - set(assets))
    if extras:
        raise PublicationError(f"release contains unexpected assets: {', '.join(extras)}")
    missing = sorted(set(assets) - set(by_name))
    if missing and not allow_missing:
        raise PublicationError(f"published release is missing assets: {', '.join(missing)}")

    for name, item in by_name.items():
        asset_id = item.get("id")
        size = item.get("size")
        local_bytes = assets[name].read_bytes()
        if item.get("state") not in (None, "uploaded"):
            raise PublicationError(f"release asset is not uploaded: {name}")
        if not isinstance(asset_id, int) or size != len(local_bytes):
            raise PublicationError(f"release asset metadata differs from intended bytes: {name}")
        if client.download_asset(asset_id) != local_bytes:
            raise PublicationError(f"release asset bytes differ from intended bytes: {name}")
    return missing


def validated_release_ids(release_ids: list[int]) -> set[int]:
    validated: set[int] = set()
    for release_id in release_ids:
        if not isinstance(release_id, int) or isinstance(release_id, bool):
            raise PublicationError("release discovery returned a non-numeric id")
        validated.add(release_id)
    return validated


def unique_matching_release_id(client: ReleaseClient, tag: str) -> int | None:
    release_ids = validated_release_ids(client.find_release_ids(tag))
    if len(release_ids) > 1:
        raise PublicationError(f"multiple releases match intended tag: {tag}")
    if not release_ids:
        return None
    return next(iter(release_ids))


def select_created_release_id(
    created_release_id: int | None, discovered_release_ids: list[int], tag: str
) -> int:
    release_ids = validated_release_ids(discovered_release_ids)
    if created_release_id is not None:
        if not isinstance(created_release_id, int) or isinstance(
            created_release_id, bool
        ):
            raise PublicationError("release creation returned no numeric id")
        release_ids.add(created_release_id)
    if len(release_ids) > 1:
        raise PublicationError(f"multiple releases match intended tag: {tag}")
    if not release_ids:
        raise PublicationError("release creation failed and no matching release exists")
    return next(iter(release_ids))


def publish_component_release(
    client: ReleaseClient,
    *,
    tag: str,
    target_commit: str,
    name: str,
    body: str,
    asset_paths: list[Path],
) -> str:
    assets = intended_assets(asset_paths)
    metadata: dict[str, object] = {
        "tag_name": tag,
        "target_commitish": target_commit,
        "name": name,
        "body": body,
        "prerelease": True,
    }
    release_id = unique_matching_release_id(client, tag)
    if release_id is not None:
        disposition = "resumed"
    else:
        created_release_id = client.create(metadata)
        release_id = select_created_release_id(
            created_release_id, client.find_release_ids(tag), tag
        )
        disposition = "created" if created_release_id is not None else "resumed"

    snapshot = client.snapshot(release_id)
    draft = snapshot.release.get("draft")
    upload_url = snapshot.release.get("upload_url")
    validated_upload_endpoint(upload_url, release_id)
    expected = {**metadata, "id": release_id, "upload_url": upload_url}
    if draft is False:
        if snapshot.release.get("immutable") is not True:
            raise PublicationError("published release is not immutable")
        validate_snapshot(client, snapshot, expected, assets, allow_missing=False)
        return "published-equivalent"
    if draft is not True:
        raise PublicationError("release draft state is invalid")

    missing = validate_snapshot(client, snapshot, expected, assets, allow_missing=True)
    for asset_name in missing:
        client.upload_asset(release_id, upload_url, assets[asset_name])

    final_snapshot = client.snapshot(release_id)
    if final_snapshot.release.get("draft") is not True:
        raise PublicationError("release draft state changed before publication")
    validate_snapshot(client, final_snapshot, expected, assets, allow_missing=False)

    # GitHub exposes no publication CAS. Repository Contents writers are trusted
    # release authorities; workflow concurrency serializes repository-owned writers.
    # These adjacent validations detect an authority breach but cannot prevent one.
    client.publish(release_id)
    published_snapshot = client.snapshot(release_id)
    if published_snapshot.release.get("draft") is not False:
        raise PublicationError(
            "post-publication authority breach: release remained a draft"
        )
    if published_snapshot.release.get("immutable") is not True:
        raise PublicationError(
            "post-publication authority breach: published release is not immutable"
        )
    try:
        validate_snapshot(
            client, published_snapshot, expected, assets, allow_missing=False
        )
    except PublicationError as error:
        raise PublicationError(
            f"post-publication authority breach: {error}"
        ) from error
    return disposition


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--target-commit", required=True)
    parser.add_argument("--name", required=True)
    parser.add_argument("--body", required=True)
    parser.add_argument("assets", nargs="+", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        result = publish_component_release(
            GhReleaseClient(),
            tag=args.tag,
            target_commit=args.target_commit,
            name=args.name,
            body=args.body,
            asset_paths=args.assets,
        )
    except (PublicationError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"component release publication refused: {error}") from error
    print(f"component_release={result} target={args.target_commit} tag={args.tag}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
