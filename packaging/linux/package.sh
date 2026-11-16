#!/bin/bash
# MoneyKeep Launcher's `cargo xtask local sync` runs this to build a Linux tarball into $DIST
# (bin/sharekeep, bin/sharekeep-hub). Linux is not a release platform yet; this is for trying it.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
VERSION="${SHAREKEEP_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}"
ARCH="$(uname -m)"
cargo build --release --locked -p sharekeep-app -p sk-hub
STAGE="$(mktemp -d)"
mkdir -p "$STAGE/bin" "$STAGE/share/icons/hicolor/512x512/apps"
cp target/release/sharekeep target/release/sharekeep-hub "$STAGE/bin/"
cp assets/icon.png "$STAGE/share/icons/hicolor/512x512/apps/sharekeep.png"
cp LICENSE-MIT LICENSE-APACHE NOTICE "$STAGE/"
DIST="${DIST:-dist}"
mkdir -p "$DIST"
tar -C "$STAGE" -czf "$DIST/sharekeep-$VERSION-linux-$ARCH.tar.gz" .
rm -rf "$STAGE"
