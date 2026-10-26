#!/usr/bin/env bash
# Build and package releases/<package version>/ for Linux and Windows using an
# isolated cross toolchain. --replace replaces that version's existing files when
# they differ; without it a differing release is kept and the script fails.
set -euo pipefail
replace=0
case "${1:-}" in
  '') ;;
  --replace) replace=1 ;;
  *) echo "usage: $0 [--replace]" >&2; exit 2 ;;
esac
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
mkdir -p target/binaries releases
epoch="$(git show -s --format=%ct HEAD)"
git diff --quiet HEAD -- Cargo.toml Cargo.lock src || {
  echo 'Commit engine source changes before packaging binaries.' >&2; exit 1;
}
docker build -f scripts/Dockerfile.binaries -t engine-iso-binaries:rust1.99 scripts
docker run --rm --user "$(id -u):$(id -g)" \
  -v "$root:/src:ro" -v "$root/target/binaries:/build" -v "$root/releases:/releases" \
  -e CARGO_HOME=/build/cargo-home -e CARGO_TARGET_DIR=/build/target \
  -e SOURCE_DATE_EPOCH="$epoch" -e REPLACE="$replace" \
  engine-iso-binaries:rust1.99 bash -euc '
    cargo build --release --locked --target x86_64-unknown-linux-gnu
    cargo build --release --locked --target x86_64-pc-windows-gnu
    cargo metadata --locked --format-version 1 > /build/metadata.json
    rustc -vV > /build/rustc.txt
    python3 scripts/package_binaries.py
  '
