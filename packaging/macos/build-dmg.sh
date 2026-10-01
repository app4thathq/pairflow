#!/usr/bin/env bash
# Assemble Pairflow.app and a compressed disk image.
# Usage: packaging/macos/build-dmg.sh /path/to/pairflow-gui [/path/to/pairflow-cli]
#
# The bundle executable is the tray app (a Mach-O), so Accessibility applies
# to Pairflow itself. pairflow-cli is optional and is for a terminal.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GUI="${1:?path to the pairflow-gui binary}"
CLI="${2:-}"
STAGE="$(mktemp -d)"
APP="$STAGE/Pairflow.app"
DIST="$ROOT/dist"

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$DIST"
cp "$ROOT/packaging/macos/Info.plist" "$APP/Contents/Info.plist"
cp "$GUI" "$APP/Contents/MacOS/pairflow-gui"
chmod +x "$APP/Contents/MacOS/pairflow-gui"
if [[ -n "$CLI" ]]; then
  cp "$CLI" "$APP/Contents/MacOS/pairflow-cli"
  chmod +x "$APP/Contents/MacOS/pairflow-cli"
fi
cp "$ROOT/packaging/macos/How-to-open.txt" "$STAGE/How to open.txt"

OUT="$DIST/pairflow-macos.dmg"
rm -f "$OUT"
hdiutil create -volname "Pairflow" -srcfolder "$STAGE" -ov -format UDZO "$OUT"
rm -rf "$STAGE"
echo "wrote $OUT"
