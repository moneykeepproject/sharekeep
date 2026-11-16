#!/bin/bash
# MoneyKeep Launcher's `cargo xtask local sync` runs this to build ShareKeep's DMG into $DIST.
#   packaging/macos/package.sh [--arch aarch64|x86_64]
# $SHAREKEEP_VERSION, when set, replaces Cargo.toml's version (dev builds).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
ARCH="$(uname -m | sed 's/arm64/aarch64/')"
while [ $# -gt 0 ]; do
  case "$1" in
    --arch) ARCH="$2"; shift 2 ;;
    *) shift ;;
  esac
done
VERSION="${SHAREKEEP_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}"
TARGET="$ARCH-apple-darwin"
cargo build --release --locked --target "$TARGET" -p sharekeep-app -p sk-hub
installer/macos/make-dmg.sh "$VERSION" "target/$TARGET/release" "$ARCH"
mkdir -p "${DIST:-dist}"
if [ -n "${DIST:-}" ]; then mv "dist/sharekeep-$VERSION-macos-$ARCH.dmg" "$DIST/"; fi
