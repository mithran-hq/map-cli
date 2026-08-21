#!/usr/bin/env python3
"""Deterministic tests for resumable immutable component publication."""

from __future__ import annotations

import copy
from email.message import Message
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from typing import Callable
import unittest
from unittest import mock


SCRIPT = Path(__file__).with_name("publish_component_release.py")
SPEC = importlib.util.spec_from_file_location("publish_component_release", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class ConditionalUnsafePatchRejectingGhClient(MODULE.GhReleaseClient):
    """Fake command boundary reproducing GitHub's captured HTTP 400."""

    CAPTURED_HTTP_400 = (
        "HTTP 400: Conditional request headers are not allowed in unsafe requests "
        "unless supported by the endpoint"
    )

    def __init__(self) -> None:
        super().__init__(endpoint="repos/example/runtime/releases")
        self.commands: list[list[str]] = []

    def _run(
        self, arguments: list[str], *, check: bool = True
    ) -> subprocess.CompletedProcess[bytes]:
        self.commands.append(arguments)
        is_unsafe_patch = "--method" in arguments and "PATCH" in arguments
        has_if_match = any(argument.startswith("If-Match: ") for argument in arguments)
        if is_unsafe_patch and has_if_match:
            raise MODULE.PublicationError(self.CAPTURED_HTTP_400)
        return subprocess.CompletedProcess(["gh", *arguments], 0, stdout=b"")

    def publish_as_old_client(self, release_id: int, etag: str) -> None:
        self._run(
            [
                "api",
                "--silent",
                "--method",
                "PATCH",
                "-H",
                f"If-Match: {etag}",
                "-F",
                "draft=false",
                f"{self.endpoint}/{release_id}",
            ]
        )


class ReleaseDiscoveryGhClient(MODULE.GhReleaseClient):
    """Fake command boundary for paginated same-tag release discovery."""

    def __init__(self, pages: object) -> None:
        super().__init__(endpoint="repos/example/runtime/releases")
        self.pages = pages
        self.commands: list[list[str]] = []

    def _run(
        self, arguments: list[str], *, check: bool = True
    ) -> subprocess.CompletedProcess[bytes]:
        self.commands.append(arguments)
        return subprocess.CompletedProcess(
            ["gh", *arguments], 0, stdout=json.dumps(self.pages).encode()
        )


class RedirectResponseOpener:
    """Return one redirect, then record any unsafe follow-up request."""

    def __init__(self, handler: MODULE.HTTPRedirectHandler) -> None:
        self.handler = handler
        self.handler.parent = self
        self.requests: list[MODULE.Request] = []

    def open(self, request: MODULE.Request, **kwargs):
        self.requests.append(request)
        response = mock.MagicMock()
        if len(self.requests) == 1:
            request.timeout = None
            headers = Message()
            headers["Location"] = "https://attacker.example/collect"
            return self.handler.http_error_302(
                request, response, 302, "Found", headers
            )
        response.__enter__.return_value.read.return_value = b"{}"
        return response


class FakeClient:
    """GitHub-like client that permits duplicate draft tags on create."""

    def __init__(
        self,
        *,
        draft: bool = True,
        snapshot_etag: str = 'W/"weak-etag-current"',
    ) -> None:
        self.release_id = 42
        self.release: dict[str, object] | None = None
        self.draft = draft
        self.snapshot_etag = snapshot_etag
        self.snapshot_count = 0
        self.create_calls = 0
        self.find_calls = 0
        self.create_result: int | None = self.release_id
        self.find_results: dict[int, list[int]] = {}
        self.matching_release_ids: list[int] = []
        self.operations: list[str] = []
        self.before_snapshot: dict[int, Callable[[FakeClient], None]] = {}
        self.after_publish: Callable[[FakeClient], None] | None = None
        self.asset_bytes: dict[int, bytes] = {}
        self.uploaded: list[str] = []
        self.uploaded_release_ids: list[int] = []
        self.uploaded_urls: list[str] = []
        self.published: list[int] = []
        self.next_asset_id = 100

    def seed(self, metadata: dict[str, object], assets: dict[str, bytes]) -> None:
        if self.release_id not in self.matching_release_ids:
            self.matching_release_ids.append(self.release_id)
        self.release = {
            **metadata,
            "id": self.release_id,
            "draft": self.draft,
            "immutable": not self.draft,
            "upload_url": (
                "https://uploads.github.com/repos/example/runtime/releases/"
                f"{self.release_id}/assets{{?name,label}}"
            ),
            "assets": [],
        }
        for name, content in assets.items():
            self._add_asset(name, content)

    def _add_asset(self, name: str, content: bytes) -> None:
        assert self.release is not None
        asset_id = self.next_asset_id
        self.next_asset_id += 1
        self.asset_bytes[asset_id] = content
        self.release["assets"].append(
            {"id": asset_id, "name": name, "size": len(content)}
        )

    def create(self, metadata: dict[str, object]) -> int | None:
        self.create_calls += 1
        self.operations.append("create")
        if self.matching_release_ids:
            self.release_id = max(self.matching_release_ids) + 1
        if self.create_result is not None:
            self.create_result = self.release_id
        self.seed(metadata, {})
        return self.create_result

    def find_release_ids(self, tag: str) -> list[int]:
        self.find_calls += 1
        self.operations.append("find")
        if self.release is not None:
            assert self.release["tag_name"] == tag
        return self.find_results.get(
            self.find_calls, self.matching_release_ids
        ).copy()

    def snapshot(self, release_id: int):
        assert release_id == self.release_id
        assert self.release is not None
        self.operations.append("snapshot")
        self.snapshot_count += 1
        mutation = self.before_snapshot.get(self.snapshot_count)
        if mutation is not None:
            mutation(self)
        return MODULE.Snapshot(self.snapshot_etag, copy.deepcopy(self.release))

    def download_asset(self, asset_id: int) -> bytes:
        return self.asset_bytes[asset_id]

    def upload_asset(self, release_id: int, upload_url: str, path: Path) -> None:
        assert self.release is not None
        assert release_id == self.release_id
        assert upload_url == self.release["upload_url"]
        self.operations.append("upload")
        self.uploaded.append(path.name)
        self.uploaded_release_ids.append(release_id)
        self.uploaded_urls.append(upload_url)
        self._add_asset(path.name, path.read_bytes())

    def publish(self, release_id: int) -> None:
        assert self.release is not None
        self.operations.append("publish")
        self.published.append(release_id)
        self.release["draft"] = False
        self.release["immutable"] = True
        if self.after_publish is not None:
            self.after_publish(self)


class PublicationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        self.assets = [root / "runtime.tar.gz", root / "runtime.tar.gz.sig"]
        self.assets[0].write_bytes(b"archive-exact-bytes")
        self.assets[1].write_bytes(b"signature-exact-bytes")
        self.kwargs = {
            "tag": "components/" + "7" * 40,
            "target_commit": "7" * 40,
            "name": "components " + "7" * 12,
            "body": "owned immutable component release",
            "asset_paths": self.assets,
        }
        self.metadata = {
            "tag_name": self.kwargs["tag"],
            "target_commitish": self.kwargs["target_commit"],
            "name": self.kwargs["name"],
            "body": self.kwargs["body"],
            "prerelease": True,
        }

    def tearDown(self) -> None:
        self.temp.cleanup()

    def test_old_conditional_client_gets_http_400_and_repaired_client_omits_header(
        self,
    ) -> None:
        client = ConditionalUnsafePatchRejectingGhClient()
        with self.assertRaisesRegex(
            MODULE.PublicationError,
            "Conditional request headers are not allowed in unsafe requests",
        ) as caught:
            client.publish_as_old_client(42, 'W/"weak-etag-current"')
        self.assertEqual(str(caught.exception), client.CAPTURED_HTTP_400)
        self.assertIn("If-Match: W/\"weak-etag-current\"", client.commands[0])

        client.publish(42)
        self.assertEqual(len(client.commands), 2)
        self.assertNotIn("-H", client.commands[1])
        self.assertNotIn("If-Match:", " ".join(client.commands[1]))

    def test_release_discovery_lists_every_exact_tag_across_pages(self) -> None:
        tag = self.kwargs["tag"]
        client = ReleaseDiscoveryGhClient(
            [
                [{"id": 40, "tag_name": "other"}, {"id": 41, "tag_name": tag}],
                [{"id": 42, "tag_name": tag}],
            ]
        )
        self.assertEqual(client.find_release_ids(tag), [41, 42])
        self.assertEqual(
            client.commands,
            [[
                "api",
                "--method",
                "GET",
                "--paginate",
                "--slurp",
                "-F",
                "per_page=100",
                "repos/example/runtime/releases",
            ]],
        )

    def test_fresh_release_accepts_weak_etag_and_validates_after_publish(self) -> None:
        client = FakeClient()
        result = MODULE.publish_component_release(client, **self.kwargs)
        self.assertEqual(result, "created")
        self.assertEqual(client.uploaded, [path.name for path in self.assets])
        self.assertEqual(client.published, [42])
        self.assertEqual(client.snapshot_count, 3)
        self.assertEqual(client.create_calls, 1)
        self.assertEqual(client.find_calls, 2)
        self.assertEqual(client.uploaded_release_ids, [42, 42])
        self.assertEqual(
            client.uploaded_urls,
            [client.release["upload_url"], client.release["upload_url"]],
        )

    def test_create_response_id_survives_empty_post_create_listing(self) -> None:
        client = FakeClient()
        client.find_results[2] = []
        result = MODULE.publish_component_release(client, **self.kwargs)
        self.assertEqual(result, "created")
        self.assertEqual(client.find_calls, 2)
        self.assertEqual(client.operations[:4], ["find", "create", "find", "snapshot"])
        self.assertEqual(client.uploaded_release_ids, [42, 42])
        self.assertEqual(client.published, [42])

    def test_failed_create_resumes_one_release_from_post_create_listing(self) -> None:
        client = FakeClient()
        client.create_result = None
        result = MODULE.publish_component_release(client, **self.kwargs)
        self.assertEqual(result, "resumed")
        self.assertEqual(client.find_calls, 2)
        self.assertEqual(client.uploaded_release_ids, [42, 42])
        self.assertEqual(client.published, [42])

    def test_failed_create_without_discovered_release_is_rejected(self) -> None:
        client = FakeClient()
        client.create_result = None
        client.find_results[2] = []
        with self.assertRaisesRegex(
            MODULE.PublicationError,
            "release creation failed and no matching release exists",
        ):
            MODULE.publish_component_release(client, **self.kwargs)
        self.assertEqual(client.snapshot_count, 0)
        self.assertEqual(client.uploaded, [])
        self.assertEqual(client.published, [])

    def test_created_and_discovered_distinct_ids_are_rejected(self) -> None:
        client = FakeClient()
        client.find_results[2] = [43]
        with self.assertRaisesRegex(
            MODULE.PublicationError, "multiple releases match intended tag"
        ):
            MODULE.publish_component_release(client, **self.kwargs)
        self.assertEqual(client.snapshot_count, 0)
        self.assertEqual(client.uploaded, [])
        self.assertEqual(client.published, [])

    def test_release_asset_upload_uses_validated_id_url_and_in_process_token(
        self,
    ) -> None:
        opener = mock.MagicMock()
        client = MODULE.GhReleaseClient(
            endpoint="repos/example/runtime/releases", upload_opener=opener
        )
        upload_url = (
            "https://uploads.github.com/repos/example/runtime/releases/"
            "42/assets{?name,label}"
        )
        response = mock.MagicMock()
        response.__enter__.return_value.read.return_value = b"{}"
        opener.open.return_value = response
        with mock.patch.dict("os.environ", {"GH_TOKEN": "secret-token"}, clear=True):
            client.upload_asset(42, upload_url, self.assets[0])

        request = opener.open.call_args.args[0]
        self.assertEqual(
            request.full_url,
            "https://uploads.github.com/repos/example/runtime/releases/42/"
            "assets?name=runtime.tar.gz",
        )
        self.assertEqual(request.method, "POST")
        self.assertEqual(request.data, self.assets[0].read_bytes())
        self.assertEqual(request.get_header("Authorization"), "Bearer secret-token")
        self.assertNotIn("secret-token", request.full_url)

    def test_release_asset_upload_refuses_cross_origin_redirect_without_token_egress(
        self,
    ) -> None:
        opener = RedirectResponseOpener(MODULE.RefuseUploadRedirects())
        client = MODULE.GhReleaseClient(
            endpoint="repos/example/runtime/releases", upload_opener=opener
        )
        upload_url = (
            "https://uploads.github.com/repos/example/runtime/releases/"
            "42/assets{?name,label}"
        )
        with mock.patch.dict("os.environ", {"GH_TOKEN": "secret-token"}, clear=True):
            with self.assertRaisesRegex(
                MODULE.PublicationError, "refused an HTTP redirect"
            ):
                client.upload_asset(42, upload_url, self.assets[0])

        self.assertEqual(len(opener.requests), 1)
        self.assertEqual(opener.requests[0].host, "uploads.github.com")
        self.assertEqual(
            opener.requests[0].get_header("Authorization"), "Bearer secret-token"
        )
        self.assertNotIn("attacker.example", opener.requests[0].full_url)

    def test_release_asset_upload_rejects_missing_token(self) -> None:
        client = MODULE.GhReleaseClient(endpoint="repos/example/runtime/releases")
        upload_url = (
            "https://uploads.github.com/repos/example/runtime/releases/"
            "42/assets{?name,label}"
        )
        with mock.patch.dict("os.environ", {}, clear=True):
            with self.assertRaisesRegex(MODULE.PublicationError, "requires GH_TOKEN"):
                client.upload_asset(42, upload_url, self.assets[0])

    def test_release_asset_upload_rejects_wrong_scheme_host_and_id(self) -> None:
        client = MODULE.GhReleaseClient(endpoint="repos/example/runtime/releases")
        invalid_urls = {
            "scheme": (
                "http://uploads.github.com/repos/example/runtime/releases/"
                "42/assets{?name,label}"
            ),
            "host": (
                "https://github.com/repos/example/runtime/releases/"
                "42/assets{?name,label}"
            ),
            "id": (
                "https://uploads.github.com/repos/example/runtime/releases/"
                "43/assets{?name,label}"
            ),
        }
        with mock.patch.dict("os.environ", {"GH_TOKEN": "secret-token"}, clear=True):
            for label, upload_url in invalid_urls.items():
                with self.subTest(label=label):
                    with self.assertRaises(MODULE.PublicationError):
                        client.upload_asset(42, upload_url, self.assets[0])

    def test_complete_equivalent_draft_is_discovered_before_create(self) -> None:
        client = FakeClient()
        client.seed(
            self.metadata,
            {path.name: path.read_bytes() for path in self.assets},
        )
        result = MODULE.publish_component_release(client, **self.kwargs)
        self.assertEqual(result, "resumed")
        self.assertEqual(client.create_calls, 0)
        self.assertEqual(client.operations[:2], ["find", "snapshot"])
        self.assertEqual(client.uploaded, [])
        self.assertEqual(client.published, [42])

    def test_multiple_same_tag_drafts_are_rejected_before_mutation(self) -> None:
        client = FakeClient()
        client.seed(self.metadata, {})
        client.matching_release_ids.append(43)
        with self.assertRaisesRegex(
            MODULE.PublicationError, "multiple releases match intended tag"
        ):
            MODULE.publish_component_release(client, **self.kwargs)
        self.assertEqual(client.create_calls, 0)
        self.assertEqual(client.snapshot_count, 0)
        self.assertEqual(client.uploaded, [])
        self.assertEqual(client.published, [])

    def test_partial_equivalent_draft_uploads_only_missing_then_publishes(self) -> None:
        client = FakeClient()
        client.seed(self.metadata, {self.assets[0].name: self.assets[0].read_bytes()})
        result = MODULE.publish_component_release(client, **self.kwargs)
        self.assertEqual(result, "resumed")
        self.assertEqual(client.uploaded, [self.assets[1].name])
        self.assertEqual(client.published, [42])

    def test_changed_draft_immediately_before_publish_is_rejected(self) -> None:
        client = FakeClient()
        client.seed(
            self.metadata,
            {path.name: path.read_bytes() for path in self.assets},
        )

        def change_body(fake: FakeClient) -> None:
            assert fake.release is not None
            fake.release["body"] = "concurrent foreign body"

        client.before_snapshot[2] = change_body
        with self.assertRaisesRegex(MODULE.PublicationError, "body does not match"):
            MODULE.publish_component_release(client, **self.kwargs)
        self.assertEqual(client.uploaded, [])
        self.assertEqual(client.published, [])

    def test_post_publication_authority_breach_byte_mismatch_is_detected(self) -> None:
        client = FakeClient()
        client.seed(
            self.metadata,
            {path.name: path.read_bytes() for path in self.assets},
        )

        def change_published_bytes(fake: FakeClient) -> None:
            assert fake.release is not None
            first_asset = fake.release["assets"][0]
            fake.asset_bytes[first_asset["id"]] = b"X" * first_asset["size"]

        client.after_publish = change_published_bytes
        with self.assertRaisesRegex(
            MODULE.PublicationError,
            "post-publication authority breach:.*bytes differ",
        ):
            MODULE.publish_component_release(client, **self.kwargs)
        self.assertEqual(client.published, [42])

    def test_post_publication_authority_breach_non_immutable_snapshot_is_detected(
        self,
    ) -> None:
        client = FakeClient()
        client.seed(
            self.metadata,
            {path.name: path.read_bytes() for path in self.assets},
        )

        def keep_mutable(fake: FakeClient) -> None:
            assert fake.release is not None
            fake.release["immutable"] = False

        client.after_publish = keep_mutable
        with self.assertRaisesRegex(
            MODULE.PublicationError,
            "post-publication authority breach:.*is not immutable",
        ):
            MODULE.publish_component_release(client, **self.kwargs)
        self.assertEqual(client.published, [42])

    def test_post_publication_authority_breach_metadata_mismatch_is_detected(
        self,
    ) -> None:
        client = FakeClient()
        client.seed(
            self.metadata,
            {path.name: path.read_bytes() for path in self.assets},
        )

        def change_published_body(fake: FakeClient) -> None:
            assert fake.release is not None
            fake.release["body"] = "foreign published body"

        client.after_publish = change_published_body
        with self.assertRaisesRegex(
            MODULE.PublicationError,
            "post-publication authority breach:.*body does not match",
        ):
            MODULE.publish_component_release(client, **self.kwargs)
        self.assertEqual(client.published, [42])

    def test_existing_asset_byte_mismatch_is_rejected_without_mutation(self) -> None:
        client = FakeClient()
        client.seed(
            self.metadata,
            {self.assets[0].name: b"X" * len(self.assets[0].read_bytes())},
        )
        with self.assertRaisesRegex(MODULE.PublicationError, "bytes differ"):
            MODULE.publish_component_release(client, **self.kwargs)
        self.assertEqual(client.uploaded, [])
        self.assertEqual(client.published, [])

    def test_extra_asset_is_rejected_without_mutation(self) -> None:
        client = FakeClient()
        client.seed(self.metadata, {"unexpected.txt": b"extra"})
        with self.assertRaisesRegex(MODULE.PublicationError, "unexpected assets"):
            MODULE.publish_component_release(client, **self.kwargs)
        self.assertEqual(client.uploaded, [])
        self.assertEqual(client.published, [])

    def test_draft_with_foreign_body_is_rejected_without_mutation(self) -> None:
        client = FakeClient()
        foreign = {**self.metadata, "body": "foreign release ownership"}
        client.seed(foreign, {})
        with self.assertRaisesRegex(MODULE.PublicationError, "body does not match"):
            MODULE.publish_component_release(client, **self.kwargs)
        self.assertEqual(client.create_calls, 0)
        self.assertEqual(client.uploaded, [])
        self.assertEqual(client.published, [])

    def test_existing_published_immutable_equivalent_resumes_without_mutation(
        self,
    ) -> None:
        client = FakeClient(draft=False)
        client.seed(
            self.metadata,
            {path.name: path.read_bytes() for path in self.assets},
        )
        result = MODULE.publish_component_release(client, **self.kwargs)
        self.assertEqual(result, "published-equivalent")
        self.assertEqual(client.snapshot_count, 1)
        self.assertEqual(client.uploaded, [])
        self.assertEqual(client.published, [])

    def test_incomplete_published_release_is_rejected_as_immutable(self) -> None:
        client = FakeClient(draft=False)
        client.seed(self.metadata, {self.assets[0].name: self.assets[0].read_bytes()})
        with self.assertRaisesRegex(MODULE.PublicationError, "is missing assets"):
            MODULE.publish_component_release(client, **self.kwargs)
        self.assertEqual(client.uploaded, [])
        self.assertEqual(client.published, [])


if __name__ == "__main__":
    unittest.main()
