#!/bin/bash
# Install or update Yplayer from its latest GitHub release:
#   /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/HaoWen46/yplayer/main/scripts/get.sh)"
# Needs macOS 26+ on Apple silicon and Homebrew; installs mpv, ffmpeg, deno
# and uv with Homebrew when missing. Safe to re-run: that is how you update.
# YPLAYER_VERSION=vX.Y.Z pins a release; YPLAYER_TARBALL=<file> installs a
# local package (with <file>.sha256 next to it) instead of downloading.
set -euo pipefail

SLUG="HaoWen46/yplayer"
ASSET="yplayer-macos-arm64.tar.gz"

say() { printf '\033[1;34m==>\033[0m \033[1m%s\033[0m\n' "$*"; }
die() { printf '\033[1;31mError:\033[0m %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = Darwin ] || die "Yplayer runs on macOS only."
OS_VERSION="$(sw_vers -productVersion)"
[ "${OS_VERSION%%.*}" -ge 26 ] || die "Yplayer needs macOS 26 or newer; this Mac has $OS_VERSION."
[ "$(uname -m)" = arm64 ] ||
    die "The download is for Apple silicon Macs. On an Intel Mac, build from source: https://github.com/$SLUG#build-from-source"

BREW="$(command -v brew || true)"
for candidate in /opt/homebrew/bin/brew /usr/local/bin/brew; do
    if [ -z "$BREW" ] && [ -x "$candidate" ]; then BREW="$candidate"; fi
done
[ -n "$BREW" ] || die "Yplayer needs Homebrew. Install it from https://brew.sh, then run this again."
PATH="$(dirname "$BREW"):$PATH"
export PATH

MISSING=()
for formula in mpv ffmpeg deno uv; do
    "$BREW" list --formula "$formula" >/dev/null 2>&1 || MISSING+=("$formula")
done
if [ "${#MISSING[@]}" -gt 0 ]; then
    say "Installing with Homebrew: ${MISSING[*]} (this can take a few minutes)"
    "$BREW" install "${MISSING[@]}"
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
if [ -n "${YPLAYER_TARBALL:-}" ]; then
    say "Using $YPLAYER_TARBALL"
    cp "$YPLAYER_TARBALL" "$TMP/$ASSET"
    cp "$YPLAYER_TARBALL.sha256" "$TMP/$ASSET.sha256"
else
    if [ -n "${YPLAYER_VERSION:-}" ]; then
        BASE="https://github.com/$SLUG/releases/download/$YPLAYER_VERSION"
    else
        BASE="https://github.com/$SLUG/releases/latest/download"
    fi
    say "Downloading Yplayer ${YPLAYER_VERSION:-(latest)}"
    curl -fL --proto '=https' --retry 2 --progress-bar -o "$TMP/$ASSET" "$BASE/$ASSET" ||
        die "couldn't download $BASE/$ASSET"
    curl -fsSL --proto '=https' --retry 2 -o "$TMP/$ASSET.sha256" "$BASE/$ASSET.sha256" ||
        die "couldn't download the checksum"
fi
(cd "$TMP" && shasum -a 256 -c -s "$ASSET.sha256") ||
    die "the download is damaged (checksum mismatch); run this again"
tar -xzf "$TMP/$ASSET" -C "$TMP"

say "Installing Yplayer $(cat "$TMP/yplayer/VERSION")"
"$TMP/yplayer/scripts/install.sh" --prebuilt "$TMP/yplayer"

echo
say "Yplayer is running: look for the ♪ in your menu bar."
echo "    Drag a YouTube link from Safari or Chrome and drop it on the orb that"
echo "    appears at the right edge of your screen."
case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) echo "    The yplay command is in ~/.local/bin; add that folder to your PATH to use it." ;;
esac
echo "    Update: run this command again. Uninstall:"
echo "    \"\$HOME/Library/Application Support/yplayer/uninstall.sh\""
