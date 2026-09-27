#!/usr/bin/env bash
set -euo pipefail

# Local landing gate for map-cli. Mirrors the declared CI workflow
# (.github/workflows/ci.yml) exactly: formatting, the full test suite,
# component packaging and the release-package tests. Run before every
# push; the pre-push hook in .githooks/ invokes this script.

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

cargo fmt --check
cargo test --locked --offline
target_dir="${CARGO_TARGET_DIR:-target}"
python3 scripts/package_component.py "$target_dir/map-cli-component.tar.gz"
python3 scripts/test_publish_component_release.py

echo "map-cli local gate passed"
