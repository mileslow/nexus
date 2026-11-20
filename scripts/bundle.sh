#!/bin/sh
# Builds dist/Nexus.app from the current source.
#
#   scripts/bundle.sh              build for this Mac
#   scripts/bundle.sh --universal  build for Apple silicon and Intel
#   scripts/bundle.sh --package    also write dist/Nexus.dmg (the download),
#                                  dist/Nexus.zip and dist/latest.json (what
#                                  the in-app updater fetches)
#   scripts/bundle.sh --install    also copy the app into /Applications
#
# NEXUS_SIGN_IDENTITY names a signing identity (for example a "Developer ID
# Application: …" certificate). Without it the app is signed ad hoc, which
# runs anywhere but is not recognised by Gatekeeper.
set -eu
cd "$(dirname "$0")/.."

universal=0
package=0
install=0
for argument in "$@"; do
    case "$argument" in
        --universal) universal=1 ;;
        --package) package=1 ;;
        --install) install=1 ;;
        *)
            echo "bundle: unknown option $argument" >&2
            exit 2
            ;;
    esac
done

version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
repo="mileslow/nexus"
export MACOSX_DEPLOYMENT_TARGET=11.0

if [ "$universal" -eq 1 ]; then
    rustup target add aarch64-apple-darwin x86_64-apple-darwin
    cargo build --release --target aarch64-apple-darwin
    cargo build --release --target x86_64-apple-darwin
    mkdir -p target/universal
    lipo -create -output target/universal/nexus \
        target/aarch64-apple-darwin/release/nexus \
        target/x86_64-apple-darwin/release/nexus
    binary=target/universal/nexus
else
    cargo build --release
    binary=target/release/nexus
fi

app=dist/Nexus.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/bin"
cp "$binary" "$app/Contents/MacOS/nexus"
cp assets/Nexus.icns "$app/Contents/Resources/Nexus.icns"
cp scripts/nexus-cli "$app/Contents/Resources/bin/nexus"
chmod +x "$app/Contents/Resources/bin/nexus"
sed "s/@VERSION@/$version/g" assets/Info.plist >"$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null

identity="${NEXUS_SIGN_IDENTITY:--}"
if [ "$identity" = "-" ]; then
    codesign --force --sign - "$app"
else
    codesign --force --options runtime --timestamp --sign "$identity" "$app"
fi
codesign --verify --deep --strict "$app"
echo "built $app ($version, $(lipo -archs "$app/Contents/MacOS/nexus"))"

if [ "$package" -eq 1 ]; then
    rm -f dist/Nexus.dmg dist/Nexus.zip dist/latest.json

    # The disk image people download: open it and drag Nexus to Applications.
    # dmgbuild lays the window out without needing Finder, so it works on CI.
    dmgbuild=target/dmgbuild/bin/dmgbuild
    if [ ! -x "$dmgbuild" ]; then
        /usr/bin/python3 -m venv target/dmgbuild
        target/dmgbuild/bin/pip install --quiet --disable-pip-version-check dmgbuild==1.6.5
    fi
    "$dmgbuild" -s scripts/dmg-settings.py \
        -D app="$app" -D icon=assets/Nexus.icns -D background=assets/dmg-background.png \
        Nexus dist/Nexus.dmg >/dev/null

    # The same app as a zip, with its checksum, for the updater and install.sh.
    ditto -c -k --keepParent "$app" dist/Nexus.zip
    sha="$(shasum -a 256 dist/Nexus.zip | cut -d ' ' -f 1)"
    cat >dist/latest.json <<JSON
{
  "version": "$version",
  "url": "https://github.com/$repo/releases/download/v$version/Nexus.zip",
  "sha256": "$sha"
}
JSON
    echo "packaged dist/Nexus.dmg, dist/Nexus.zip and dist/latest.json"
fi

if [ "$install" -eq 1 ]; then
    destination="${NEXUS_INSTALL_DIR:-/Applications}"
    # Replace by renaming, so a running copy keeps working until it quits.
    if [ -e "$destination/Nexus.app" ]; then
        rm -rf "$destination/.Nexus.app.previous"
        mv "$destination/Nexus.app" "$destination/.Nexus.app.previous"
    fi
    ditto "$app" "$destination/Nexus.app"
    rm -rf "$destination/.Nexus.app.previous"
    echo "installed $destination/Nexus.app"
fi
