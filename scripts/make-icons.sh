#!/usr/bin/env bash
# Renders every committed icon file from assets/icon.svg, the one source of the app icon.
# Run it after changing the SVG (macOS: it draws with AppKit and iconutil). Each size is
# drawn from the SVG, so small sizes stay sharp.
set -euo pipefail
cd "$(dirname "$0")/.."
SVG=assets/icon.svg
render() { swift scripts/render-icon.swift "$SVG" "$2" "$1"; }
render 1024 assets/icon_1024.png
render 256 assets/icon_256.png
SET="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$SET"
for s in 16 32 128 256 512; do
  render "$s" "$SET/icon_${s}x${s}.png"
  render $((s * 2)) "$SET/icon_${s}x${s}@2x.png"
done
iconutil -c icns "$SET" -o assets/AppIcon.icns
rm -rf "$(dirname "$SET")"
for s in 16 24 32 48 64 128 256 512; do
  render "$s" "packaging/linux/icons/hicolor/${s}x${s}/apps/ternaryshell.png"
done
python3 scripts/make-windows-icon.py "$SVG" packaging/windows/ternaryshell.ico
