#!/usr/bin/env python3
"""Build a MAP CLI component artifact for Aegis.pkg package proof."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
COMPONENT = "map-cli"
BINARY = "map"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    args = parser.parse_args()

    subprocess.run(["cargo", "build", "--release"], cwd=ROOT, check=True)
    binary = ROOT / "target" / "release" / BINARY
    version = cargo_version()
    source_ref = git_ref()

    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp) / COMPONENT
        bin_dir = root / "bin"
        bin_dir.mkdir(parents=True)
        shutil.copy2(binary, bin_dir / BINARY)
        manifest = {
            "schema_version": "aegis.component.v1",
            "component": COMPONENT,
            "version": version,
            "source_ref": f"git:{source_ref}",
            "license": {"expression": "Apache-2.0"},
            "binaries": [
                {
                    "name": BINARY,
                    "path": f"bin/{BINARY}",
                    "sha256": sha256(bin_dir / BINARY),
                }
            ],
            "signing": {
                "code_signed": False,
                "notarized": False,
                "state": "unsigned_component_artifact",
            },
        }
        (root / "manifest.json").write_text(
            json.dumps(manifest, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        args.output.parent.mkdir(parents=True, exist_ok=True)
        with tarfile.open(args.output, "w:gz") as tar:
            tar.add(root, arcname=COMPONENT)


def cargo_version() -> str:
    metadata = subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        text=True,
    )
    return json.loads(metadata)["packages"][0]["version"]


def git_ref() -> str:
    try:
        return subprocess.check_output(
            ["git", "rev-parse", "HEAD"],
            cwd=ROOT,
            text=True,
            stderr=subprocess.DEVNULL,
        ).strip()
    except subprocess.CalledProcessError:
        return os.environ.get("GITHUB_SHA", "unknown")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


if __name__ == "__main__":
    main()
