#!/usr/bin/env python3
"""Package the MAP CLI (`map`) as a durable Aegis host component artifact.

`map` is the public hosted-operations client bundled into Aegis.app and shimmed
to /usr/local/bin/map (ADR-0005). Unlike the guest/worker components it is a HOST
(mach-o) binary, so it has its own producer here and is consumed on the aegis side
by package_proof, which stages it into Aegis.app and resolves it via the component
manifest. The Daily Dogfood Release pins this artifact by tag + sha256
(aegis ADR-0022 / aegis#980).

Layout mirrors the aegis-agent-runtime host-bridge component so the aegis
assembler can consume both identically:

    map-cli-component/manifest.json
    map-cli-component/bin/map

Emitted artifact: <output-dir>/map-cli-<version>-macos-<arch>.tar.gz
"""
import argparse
import gzip
import hashlib
import io
import json
import os
import subprocess
import tarfile
from datetime import datetime, timezone
from pathlib import Path

SCHEMA_VERSION = "aegis.map_cli.host_component.v1"
COMPONENT = "map-cli"
BINARY = "map"
HOST_MACOS_TARGETS = {("macos", "aarch64")}  # arm64-only, fail-closed (aegis ADR-0034)
ARCH_ALIASES = {"amd64": "x86_64", "arm64": "aarch64"}


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def normalize_arch(value: str) -> str:
    return ARCH_ALIASES.get(value.strip().lower(), value.strip().lower())


def created_at_from_environment() -> str:
    # Deterministic when SOURCE_DATE_EPOCH is set (reproducible CI artifacts).
    epoch = os.environ.get("SOURCE_DATE_EPOCH")
    if epoch and epoch.isdigit():
        return datetime.fromtimestamp(int(epoch), tz=timezone.utc).isoformat()
    return datetime.now(tz=timezone.utc).isoformat()


def version_evidence(binary: Path, target_arch: str) -> dict:
    """Probe `--help` only when the artifact arch matches the build host;
    a cross-arch mach-o cannot run here, so record an honest unavailability."""
    host_arch = normalize_arch(os.uname().machine)
    if host_arch != target_arch:
        return {
            "kind": "version_probe_unavailable",
            "reason": f"cross-arch artifact ({target_arch}) not runnable on host ({host_arch})",
        }
    try:
        out = subprocess.check_output(
            [str(binary), "--help"], text=True, stderr=subprocess.STDOUT, timeout=10
        )
    except Exception as error:  # noqa: BLE001 - best-effort evidence only
        return {"kind": "version_probe_failed", "error": str(error)}
    line = out.splitlines()[0].strip() if out.strip() else ""
    return {
        "kind": "host_executed_probe",
        "command": [f"bin/{BINARY}", "--help"],
        "stdout_first_line": line,
        "stdout_sha256": sha256_bytes(out.encode("utf-8")),
    }


def deterministic_tar_gz(entries: list[tuple[str, bytes, int]], out_path: Path) -> None:
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w") as tar:
        for name, data, mode in sorted(entries, key=lambda item: item[0]):
            info = tarfile.TarInfo(name=name)
            info.size = len(data)
            info.mode = mode
            info.mtime = 0
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            tar.addfile(info, io.BytesIO(data))
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with out_path.open("wb") as handle:
        with gzip.GzipFile(fileobj=handle, mode="wb", mtime=0) as gz:
            gz.write(raw.getvalue())


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True, help="Built `map` binary.")
    parser.add_argument("--target-arch", required=True, help="aarch64 (or arm64).")
    parser.add_argument("--version", required=True, help="Version slug for the manifest (e.g. git short sha).")
    parser.add_argument("--source-ref", required=True, help="Provenance ref, e.g. git:<full-sha>.")
    parser.add_argument("--signing-state", default="unsigned", help="unsigned | adhoc | developer-id.")
    parser.add_argument("--output-dir", type=Path, default=Path("dist/map-cli"))
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    target_arch = normalize_arch(args.target_arch)
    if ("macos", target_arch) not in HOST_MACOS_TARGETS:
        allowed = ", ".join(sorted(f"macos-{a}" for _, a in HOST_MACOS_TARGETS))
        raise SystemExit(
            f"unsupported map-cli target: macos-{target_arch}; expected one of: {allowed}"
        )

    binary_bytes = args.binary.read_bytes()
    binary_mode = (args.binary.stat().st_mode & 0o777) | 0o111  # ensure executable
    version_slug = args.version.strip()
    artifact_name = f"{COMPONENT}-{version_slug}-macos-{target_arch}"

    manifest = {
        "schema_version": SCHEMA_VERSION,
        "component": COMPONENT,
        "kind": "hosted_operations_cli",
        "public_command": BINARY,
        "shim_path": "/usr/local/bin/map",
        "version": version_slug,
        "source_ref": args.source_ref,
        "target": {"os": "macos", "arch": target_arch},
        "binary": {
            "path": f"bin/{BINARY}",
            "sha256": sha256_bytes(binary_bytes),
            "size_bytes": len(binary_bytes),
            "mode": f"{binary_mode:04o}",
            "binary_format": "mach-o",
        },
        "signing": {"state": args.signing_state},
        "version_evidence": version_evidence(args.binary, target_arch),
        "raw_credential_material_present": False,
        "created_at": created_at_from_environment(),
    }
    manifest_bytes = json.dumps(manifest, indent=2, sort_keys=True).encode("utf-8") + b"\n"

    args.output_dir.mkdir(parents=True, exist_ok=True)
    tar_path = args.output_dir / f"{artifact_name}.tar.gz"
    deterministic_tar_gz(
        [
            (f"{COMPONENT}-component/manifest.json", manifest_bytes, 0o644),
            (f"{COMPONENT}-component/bin/{BINARY}", binary_bytes, binary_mode),
        ],
        tar_path,
    )
    print(
        json.dumps(
            {
                "schema_version": SCHEMA_VERSION,
                "artifact": str(tar_path),
                "artifact_sha256": sha256_file(tar_path),
                "manifest": manifest,
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
