#!/usr/bin/env bash
# Install `yplay serve` as the LaunchAgent com.yplayer.service and the menu-bar
# app as the LaunchAgent com.yplayer.app.
#   scripts/install.sh                   build from this checkout (developers)
#   install.sh --prebuilt <dir>          install an unpacked release package
#                                        (scripts/get.sh runs this)
# YPLAYER_NO_LAUNCHD=1 lays the files down without touching launchd (tests).
# Idempotent: safe to re-run after pulling changes or to update.
set -euo pipefail

PREBUILT=""
if [ "${1:-}" = "--prebuilt" ]; then
    PREBUILT="$(cd "${2:?usage: install.sh --prebuilt <dir>}" && pwd)"
fi

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# packaging/ templates: in the checkout, or in the unpacked package.
SRC="${PREBUILT:-$REPO}"
LABEL="com.yplayer.service"
DOMAIN="gui/$UID"
BIN="$HOME/.local/bin/yplay"
STATE="$HOME/Library/Application Support/yplayer"
CONFIG="$STATE/config.toml"
LOG="$HOME/Library/Logs/yplayer/service.log"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
APP_LABEL="com.yplayer.app"
APP="$HOME/Applications/Yplayer.app"
APP_LOG="$HOME/Library/Logs/yplayer/app.log"
APP_PLIST="$HOME/Library/LaunchAgents/$APP_LABEL.plist"

launchd() { [ -z "${YPLAYER_NO_LAUNCHD:-}" ]; }

# Stop a LaunchAgent if loaded; bootout returns before the job is gone and
# bootstrap fails while it lingers.
stop_agent() {
    launchd || return 0
    echo "==> stopping $DOMAIN/$1 (if loaded)"
    launchctl bootout "$DOMAIN/$1" 2>/dev/null || true
    for _ in $(seq 50); do
        launchctl print "$DOMAIN/$1" >/dev/null 2>&1 || break
        sleep 0.1
    done
}

start_agent() {
    launchd || return 0
    echo "==> starting $DOMAIN/$1"
    launchctl bootstrap "$DOMAIN" "$2"
    launchctl print "$DOMAIN/$1" | grep -E '^[[:space:]]*(state|pid|last exit code) =' || true
}

mkdir -p "$(dirname "$BIN")" "$STATE"
chmod 700 "$STATE"

if [ -n "$PREBUILT" ]; then
    echo "==> installing $BIN"
    install -m 0755 "$PREBUILT/bin/yplay" "$BIN"

    # The worker gets its own venv on a uv-managed Python, so Homebrew Python
    # upgrades never break it.
    VENV="$STATE/venv"
    PYTHON="$VENV/bin/python3"
    echo "==> worker venv $VENV"
    uv venv --quiet --allow-existing --managed-python --python 3.12 "$VENV"
    uv pip install --quiet --python "$PYTHON" --reinstall-package yplayer "$PREBUILT"/worker/yplayer-*.whl
    install -m 0755 "$PREBUILT/scripts/uninstall.sh" "$STATE/uninstall.sh"
else
    cd "$REPO"
    echo "==> cargo build --release"
    cargo build --release
    echo "==> installing $BIN"
    install -m 0755 target/release/yplay "$BIN"
    echo "==> uv pip install -e . (worker)"
    uv pip install --python .venv/bin/python -e .
    PYTHON="$REPO/.venv/bin/python3"
fi

# worker_python: a checkout keeps a value already set; a package always points
# it at its own venv.
if [ -f "$CONFIG" ] && grep -Eq '^[[:space:]]*worker_python[[:space:]]*=' "$CONFIG"; then
    if [ -n "$PREBUILT" ]; then
        echo "==> setting worker_python = \"$PYTHON\" in $CONFIG"
        grep -Ev '^[[:space:]]*worker_python[[:space:]]*=' "$CONFIG" >"$CONFIG.tmp" || true
        { printf 'worker_python = "%s"\n' "$PYTHON"; cat "$CONFIG.tmp"; } >"$CONFIG.new"
        rm -f "$CONFIG.tmp"
        mv "$CONFIG.new" "$CONFIG"
    else
        echo "==> $CONFIG already sets worker_python (unchanged)"
    fi
else
    echo "==> adding worker_python = \"$PYTHON\" to $CONFIG"
    # Prepend so the key stays top-level even if the file ends in a [table].
    {
        printf 'worker_python = "%s"\n' "$PYTHON"
        if [ -f "$CONFIG" ]; then cat "$CONFIG"; fi
    } >"$CONFIG.tmp"
    mv "$CONFIG.tmp" "$CONFIG"
fi

echo "==> rendering $PLIST"
mkdir -p "$(dirname "$PLIST")" "$(dirname "$LOG")"
sed -e "s|@BIN@|$BIN|g" -e "s|@LOG@|$LOG|g" \
    "$SRC/packaging/$LABEL.plist.in" >"$PLIST"

stop_agent "$LABEL"

# Back up the library DB once, before the service first opens (and migrates) it.
CACHE_DIR="$(sed -nE "s/^[[:space:]]*cache_dir[[:space:]]*=[[:space:]]*[\"']([^\"']*)[\"'].*/\1/p" "$CONFIG" | head -n 1)"
CACHE_DIR="${CACHE_DIR:-$HOME/Music/yt-audio}"
DB="$CACHE_DIR/.yplayer.db"
BAK="$CACHE_DIR/.yplayer.db.pre-service.bak"
if [ ! -f "$DB" ]; then
    echo "==> no library DB at $DB (nothing to back up)"
elif [ -e "$BAK" ]; then
    echo "==> backup already exists: $BAK (not overwritten)"
else
    # sqlite3 .backup also captures changes still in the -wal file; cp would not.
    if command -v sqlite3 >/dev/null 2>&1; then
        (umask 077 && sqlite3 "$DB" ".backup '$BAK'")
    else
        (umask 077 && cp "$DB" "$BAK")
    fi
    chmod 600 "$BAK"*
    echo "==> backed up $DB -> $BAK"
fi

start_agent "$LABEL" "$PLIST"
echo "Log: $LOG"

if [ -n "$PREBUILT" ]; then
    BUILT="$PREBUILT/Yplayer.app"
else
    echo "==> building the menu-bar app"
    BUILT="$("$REPO/scripts/build-app.sh")"
fi

echo "==> staging $APP.tmp"
mkdir -p "$(dirname "$APP")"
rm -rf "$APP.tmp"
ditto "$BUILT" "$APP.tmp"
# A package fetched with a browser is quarantined; the app is ad-hoc signed.
xattr -dr com.apple.quarantine "$APP.tmp" "$BIN" 2>/dev/null || true

echo "==> rendering $APP_PLIST"
sed -e "s|@BIN@|$APP/Contents/MacOS/Yplayer|g" -e "s|@LOG@|$APP_LOG|g" \
    "$SRC/packaging/$APP_LABEL.plist.in" >"$APP_PLIST"

stop_agent "$APP_LABEL"

echo "==> installing $APP"
rm -rf "$APP"
mv "$APP.tmp" "$APP"

start_agent "$APP_LABEL" "$APP_PLIST"
echo "Log: $APP_LOG"
