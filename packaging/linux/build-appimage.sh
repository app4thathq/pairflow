#!/usr/bin/env bash
# Build a Pairflow AppImage from target/release/pairflow.
# Run on Linux after `cargo build --release`. FUSE is not required.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="${1:-$ROOT/target/release/pairflow}"
APPDIR="$ROOT/packaging/linux/AppDir"
DIST="$ROOT/dist"
TOOL_DIR="${TMPDIR:-/tmp}/pairflow-appimagetool"

if [[ ! -x "$BIN" ]]; then
  echo "missing release binary at $BIN" >&2
  exit 1
fi

rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$DIST"
cp "$BIN" "$APPDIR/usr/bin/pairflow"
chmod +x "$APPDIR/usr/bin/pairflow"
cp "$ROOT/packaging/linux/pairflow.desktop" "$APPDIR/pairflow.desktop"
cp "$ROOT/packaging/linux/pairflow.png" "$APPDIR/pairflow.png"

cat > "$APPDIR/AppRun" << 'EOF'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
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
