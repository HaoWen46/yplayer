#!/usr/bin/env bash
# Build the menu-bar app and assemble an ad-hoc signed build/Yplayer.app.
# Prints the bundle path on stdout; build output goes to stderr.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PKG="$REPO/apps/macos"
APP="$REPO/build/Yplayer.app"

swift build -c release --package-path "$PKG" >&2
BIN_DIR="$(swift build -c release --package-path "$PKG" --show-bin-path)"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN_DIR/Yplayer" "$APP/Contents/MacOS/Yplayer"
cp "$PKG/Packaging/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"
cp "$PKG/Packaging/Info.plist" "$APP/Contents/Info.plist"
codesign --force --sign - "$APP" >&2

echo "$APP"
