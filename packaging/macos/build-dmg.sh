#!/usr/bin/env bash
# Assemble Pairflow.app and a compressed disk image.
# Usage: packaging/macos/build-dmg.sh /path/to/pairflow-binary
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="${1:?path to the pairflow binary}"
STAGE="$(mktemp -d)"
APP="$STAGE/Pairflow.app"
DIST="$ROOT/dist"

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$DIST"
cp "$ROOT/packaging/macos/Info.plist" "$APP/Contents/Info.plist"
cp "$BIN" "$APP/Contents/MacOS/pairflow-cli"
cp "$ROOT/packaging/macos/launcher.sh" "$APP/Contents/MacOS/pairflow-launcher"
cp "$ROOT/packaging/macos/How-to-open.txt" "$STAGE/How to open.txt"
chmod +x "$APP/Contents/MacOS/pairflow-cli" "$APP/Contents/MacOS/pairflow-launcher"

OUT="$DIST/pairflow-macos.dmg"
rm -f "$OUT"
hdiutil create -volname "Pairflow" -srcfolder "$STAGE" -ov -format UDZO "$OUT"
rm -rf "$STAGE"
echo "wrote $OUT"
