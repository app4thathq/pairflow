#!/usr/bin/env bash
# Build a Pairflow AppImage from the release binaries.
# Run on Linux after `cargo build --release`. FUSE is not required.
# No arguments launches the tray app. Any arguments run the CLI
# (`pairflow host`, `pairflow join CODE`, and so on).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GUI="${1:-$ROOT/target/release/pairflow-gui}"
CLI="${2:-$ROOT/target/release/pairflow}"
APPDIR="$ROOT/packaging/linux/AppDir"
DIST="$ROOT/dist"
TOOL_DIR="${TMPDIR:-/tmp}/pairflow-appimagetool"

if [[ ! -x "$GUI" ]]; then
  echo "missing tray binary at $GUI" >&2
  exit 1
fi
if [[ ! -x "$CLI" ]]; then
  echo "missing CLI binary at $CLI" >&2
  exit 1
fi

rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$DIST"
cp "$GUI" "$APPDIR/usr/bin/pairflow-gui"
cp "$CLI" "$APPDIR/usr/bin/pairflow"
chmod +x "$APPDIR/usr/bin/pairflow-gui" "$APPDIR/usr/bin/pairflow"
cp "$ROOT/packaging/linux/pairflow.desktop" "$APPDIR/pairflow.desktop"
cp "$ROOT/packaging/linux/pairflow.png" "$APPDIR/pairflow.png"

cat > "$APPDIR/AppRun" << 'EOF'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
if [ "$#" -eq 0 ]; then
  exec "$HERE/usr/bin/pairflow-gui"
fi
exec "$HERE/usr/bin/pairflow" "$@"
EOF
chmod +x "$APPDIR/AppRun"

if [[ ! -x "$TOOL_DIR/squashfs-root/AppRun" ]]; then
  mkdir -p "$TOOL_DIR"
  curl -fsSL -o "$TOOL_DIR/appimagetool.AppImage" \
    https://github.com/AppImage/AppImageKit/releases/download/continuous/appimagetool-x86_64.AppImage
  chmod +x "$TOOL_DIR/appimagetool.AppImage"
  (cd "$TOOL_DIR" && ./appimagetool.AppImage --appimage-extract)
fi

ARCH=x86_64 "$TOOL_DIR/squashfs-root/AppRun" "$APPDIR" "$DIST/pairflow-linux-x86_64.AppImage"
chmod +x "$DIST/pairflow-linux-x86_64.AppImage"
echo "wrote $DIST/pairflow-linux-x86_64.AppImage"
