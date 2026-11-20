#!/bin/sh
# Installs the latest Nexus release:
#
#   curl -fsSL https://raw.githubusercontent.com/mileslow/nexus/main/install.sh | sh
#
# NEXUS_INSTALL_DIR chooses where the app goes (default /Applications).
set -eu

repo="mileslow/nexus"
base="https://github.com/$repo/releases/latest/download"
destination="${NEXUS_INSTALL_DIR:-/Applications}"

[ "$(uname -s)" = "Darwin" ] || {
    echo "install: Nexus is a macOS app" >&2
    exit 1
}

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

echo "Downloading Nexus…"
curl -fsSL -o "$work/latest.json" "$base/latest.json"
version="$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' "$work/latest.json")"
expected="$(sed -n 's/.*"sha256": *"\([^"]*\)".*/\1/p' "$work/latest.json")"
curl -fL --progress-bar -o "$work/Nexus.zip" "https://github.com/$repo/releases/download/v$version/Nexus.zip"

actual="$(shasum -a 256 "$work/Nexus.zip" | cut -d ' ' -f 1)"
[ "$actual" = "$expected" ] || {
    echo "install: the download did not match its checksum" >&2
    exit 1
}

ditto -x -k "$work/Nexus.zip" "$work/unpacked"
codesign --verify --deep --strict "$work/unpacked/Nexus.app"

mkdir -p "$destination"
# Replace by renaming, so a running copy keeps working until it quits.
if [ -e "$destination/Nexus.app" ]; then
    rm -rf "$destination/.Nexus.app.previous"
    mv "$destination/Nexus.app" "$destination/.Nexus.app.previous"
fi
mv "$work/unpacked/Nexus.app" "$destination/Nexus.app"
rm -rf "$destination/.Nexus.app.previous"
echo "Installed Nexus $version in $destination"

# Put the `nexus` command on the PATH if there is somewhere to put it.
cli="$destination/Nexus.app/Contents/Resources/bin/nexus"
for bin in /usr/local/bin "$HOME/.local/bin"; do
    if [ -d "$bin" ] && [ -w "$bin" ]; then
        ln -sf "$cli" "$bin/nexus"
        echo "Linked the nexus command into $bin"
        break
    fi
done

echo "Open it from Applications, or run: open -a Nexus"
