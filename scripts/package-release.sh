#!/usr/bin/env bash
# Build the release package dist/yplayer-macos-arm64.tar.gz (+ .sha256):
# yplay, Yplayer.app, the worker wheel, LaunchAgent templates and the
# install/uninstall scripts. The release workflow runs this; so can you.
# Usage: scripts/package-release.sh [vX.Y.Z]   (a tag must match the versions)
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"
NAME="yplayer-macos-arm64"

die() { echo "error: $*" >&2; exit 1; }

[ "$(uname -m)" = arm64 ] || die "the package is built for Apple silicon; run this on an arm64 Mac"

VERSION="$(sed -nE 's/^version = "(.*)"/\1/p' crates/yplayer/Cargo.toml | head -n 1)"
PY_VERSION="$(sed -nE 's/^version = "(.*)"/\1/p' pyproject.toml | head -n 1)"
APP_VERSION="$(plutil -extract CFBundleShortVersionString raw apps/macos/Packaging/Info.plist)"
INIT_VERSION="$(sed -nE 's/^__version__ = "(.*)"/\1/p' yplayer/__init__.py)"
[ "$VERSION" = "$PY_VERSION" ] && [ "$VERSION" = "$APP_VERSION" ] && [ "$VERSION" = "$INIT_VERSION" ] ||
    die "versions differ: Cargo.toml $VERSION, pyproject.toml $PY_VERSION, Info.plist $APP_VERSION, __init__.py $INIT_VERSION"
if [ -n "${1:-}" ] && [ "$1" != "v$VERSION" ]; then
    die "tag $1 does not match version $VERSION"
fi

echo "==> cargo build --release" >&2
cargo build --release --locked >&2
echo "==> building Yplayer.app" >&2
APP="$(scripts/build-app.sh)"

OUT="dist/yplayer"
rm -rf dist
mkdir -p "$OUT/bin" "$OUT/worker" "$OUT/scripts" "$OUT/packaging"
install -m 0755 target/release/yplay "$OUT/bin/yplay"
ditto "$APP" "$OUT/Yplayer.app"
echo "==> building the worker wheel" >&2
uv build --quiet --wheel --out-dir "$OUT/worker" . >&2
rm -f "$OUT/worker/.gitignore"
install -m 0755 scripts/install.sh scripts/uninstall.sh "$OUT/scripts/"
cp packaging/*.plist.in "$OUT/packaging/"
cp LICENSE "$OUT/"
echo "$VERSION" >"$OUT/VERSION"

# No AppleDouble (._*) files or extended attributes in the archive.
tar --no-mac-metadata -C dist -czf "dist/$NAME.tar.gz" yplayer
(cd dist && shasum -a 256 "$NAME.tar.gz" >"$NAME.tar.gz.sha256")
echo "==> dist/$NAME.tar.gz ($VERSION)" >&2
