#!/bin/sh
# Publishes a new version of Nexus.
#
#   scripts/release.sh patch     0.2.0 -> 0.2.1
#   scripts/release.sh minor     0.2.0 -> 0.3.0
#   scripts/release.sh major     0.2.0 -> 1.0.0
#   scripts/release.sh 0.4.2     that exact version
#
# It bumps the version, commits, tags and pushes. GitHub Actions then builds
# the app and publishes the release; running copies of Nexus pick it up on
# their next update check.
set -eu
cd "$(dirname "$0")/.."

[ "$#" -eq 1 ] || {
    echo "usage: scripts/release.sh patch|minor|major|X.Y.Z" >&2
    exit 2
}

current="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
major="${current%%.*}"
rest="${current#*.}"
minor="${rest%%.*}"
patch="${rest#*.}"

case "$1" in
    patch) next="$major.$minor.$((patch + 1))" ;;
    minor) next="$major.$((minor + 1)).0" ;;
    major) next="$((major + 1)).0.0" ;;
    *)
        next="${1#v}"
        echo "$next" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || {
            echo "release: '$1' is not a version like 1.2.3" >&2
            exit 2
        }
        ;;
esac

branch="$(git rev-parse --abbrev-ref HEAD)"
[ "$branch" = "main" ] || {
    echo "release: releases are cut from main; this is $branch" >&2
    exit 1
}
[ -z "$(git status --porcelain)" ] || {
    echo "release: commit or stash your changes first" >&2
    exit 1
}
git fetch --quiet origin main
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || {
    echo "release: main is not in sync with origin; pull or push first" >&2
    exit 1
}
if git rev-parse --quiet --verify "refs/tags/v$next" >/dev/null; then
    echo "release: v$next already exists" >&2
    exit 1
fi

if [ "$next" != "$current" ]; then
    # Only the package's own version line, which comes first in both files.
    sed -i '' "1,/^version = \"$current\"/s/^version = \"$current\"/version = \"$next\"/" Cargo.toml
    cargo update --quiet --offline --package nexus
    git add Cargo.toml Cargo.lock
    git commit --quiet -m "Release $next"
fi
git tag "v$next"
git push --quiet origin main "v$next"

echo "pushed v$next; GitHub Actions is building it:"
echo "  https://github.com/mileslow/nexus/actions/workflows/release.yml"
