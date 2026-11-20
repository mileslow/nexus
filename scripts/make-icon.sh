#!/bin/sh
# Rebuilds the app icon from assets/icon.svg.
# Needs rsvg-convert (`brew install librsvg`); iconutil ships with macOS.
set -eu
cd "$(dirname "$0")/.."

command -v rsvg-convert >/dev/null || {
    echo "make-icon: rsvg-convert not found; run: brew install librsvg" >&2
    exit 1
}

iconset="$(mktemp -d)/Nexus.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    double=$((size * 2))
    rsvg-convert -w "$size" -h "$size" assets/icon.svg -o "$iconset/icon_${size}x${size}.png"
    rsvg-convert -w "$double" -h "$double" assets/icon.svg -o "$iconset/icon_${size}x${size}@2x.png"
done
iconutil --convert icns --output assets/Nexus.icns "$iconset"
# The copy the app draws on its home view and the README shows.
rsvg-convert -w 256 -h 256 assets/icon.svg -o assets/icon.png
rm -rf "$(dirname "$iconset")"
echo "wrote assets/Nexus.icns and assets/icon.png"
