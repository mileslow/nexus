#!/bin/sh
# Rebuilds the app icon from assets/icon.svg (for the Mac and for Windows),
# and the installer disk image's backdrop from assets/dmg-background.svg.
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
# The same picture for Windows, at the sizes Explorer and the taskbar ask for.
for size in 16 24 32 48 64 256; do
    rsvg-convert -w "$size" -h "$size" assets/icon.svg -o "$iconset/ico_$size.png"
done
/usr/bin/python3 scripts/make-ico.py assets/Nexus.ico \
    "$iconset/ico_16.png" "$iconset/ico_24.png" "$iconset/ico_32.png" \
    "$iconset/ico_48.png" "$iconset/ico_64.png" "$iconset/ico_256.png"
# The copy the app draws on its home view and the README shows.
rsvg-convert -w 256 -h 256 assets/icon.svg -o assets/icon.png
rm -rf "$(dirname "$iconset")"
# The backdrop of the installer disk image, at both screen densities.
rsvg-convert -w 640 -h 400 assets/dmg-background.svg -o assets/dmg-background.png
rsvg-convert -w 1280 -h 800 assets/dmg-background.svg -o "assets/dmg-background@2x.png"
echo "wrote assets/Nexus.icns, assets/Nexus.ico, assets/icon.png and the disk image backdrop"
