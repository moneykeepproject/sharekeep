#!/bin/bash
# Build ShareKeep.app and sharekeep-<version>-macos-<arch>.dmg (run on a Mac, after
# `cargo build --release -p sharekeep-app -p sk-hub`).
#
#   installer/macos/make-dmg.sh 0.1.0 [target/release] [arch]
#
# The app is signed ad hoc (no Developer ID yet, as the other Keep apps): the first time, open
# it with right-click > Open. The DMG also holds "Install the household hub.command", for the
# one Mac that keeps the household's books, and "Stop the household hub.command".
set -euo pipefail
VERSION="${1:?version, like 0.1.0}"
BIN="${2:-target/release}"
ARCH="${3:-$(uname -m | sed 's/x86_64/x86_64/; s/arm64/aarch64/')}"
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
OUT="$ROOT/dist"
STAGE="$OUT/dmg"
APP="$STAGE/ShareKeep.app"
rm -rf "$STAGE"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN/sharekeep" "$BIN/sharekeep-hub" "$APP/Contents/MacOS/"
cp "$ROOT/assets/icon.icns" "$APP/Contents/Resources/ShareKeep.icns"
sed "s/@VERSION@/$VERSION/g" "$HERE/Info.plist" > "$APP/Contents/Info.plist"
cp "$HERE/install-hub.command" "$STAGE/Install the household hub.command"
chmod +x "$STAGE/Install the household hub.command"
cp "$HERE/uninstall-hub.command" "$STAGE/Stop the household hub.command"
chmod +x "$STAGE/Stop the household hub.command"
cp "$ROOT/LICENSE-MIT" "$ROOT/LICENSE-APACHE" "$ROOT/NOTICE" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
codesign --force --deep --sign - "$APP"
# MoneyKeep Launcher finds it by this name
DMG="$OUT/sharekeep-$VERSION-macos-$ARCH.dmg"
rm -f "$DMG"
hdiutil create -volname "ShareKeep $VERSION" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
shasum -a 256 "$DMG"
