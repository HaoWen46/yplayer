#!/usr/bin/env bash
# Build yplay and install `yplay serve` as the LaunchAgent com.yplayer.service;
# build the menu-bar app and install it as the LaunchAgent com.yplayer.app.
# Idempotent: safe to re-run after pulling changes.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LABEL="com.yplayer.service"
DOMAIN="gui/$UID"
BIN="$HOME/.local/bin/yplay"
PYTHON="$REPO/.venv/bin/python3"
CONFIG="$HOME/Library/Application Support/yplayer/config.toml"
LOG="$HOME/Library/Logs/yplayer/service.log"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
APP_LABEL="com.yplayer.app"
APP="$HOME/Applications/Yplayer.app"
APP_LOG="$HOME/Library/Logs/yplayer/app.log"
APP_PLIST="$HOME/Library/LaunchAgents/$APP_LABEL.plist"

cd "$REPO"

echo "==> cargo build --release"
cargo build --release

echo "==> installing $BIN"
mkdir -p "$(dirname "$BIN")"
install -m 0755 target/release/yplay "$BIN"

echo "==> uv pip install -e . (worker)"
uv pip install --python .venv/bin/python -e .

mkdir -p "$(dirname "$CONFIG")"
if [ -f "$CONFIG" ] && grep -Eq '^[[:space:]]*worker_python[[:space:]]*=' "$CONFIG"; then
    echo "==> $CONFIG already sets worker_python (unchanged)"
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
    "$REPO/packaging/$LABEL.plist.in" >"$PLIST"

echo "==> stopping $DOMAIN/$LABEL (if loaded)"
launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
# bootout returns before the job is gone; bootstrap fails while it lingers.
for _ in $(seq 50); do
    launchctl print "$DOMAIN/$LABEL" >/dev/null 2>&1 || break
    sleep 0.1
done

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

echo "==> starting $DOMAIN/$LABEL"
launchctl bootstrap "$DOMAIN" "$PLIST"

echo "==> status"
launchctl print "$DOMAIN/$LABEL" | grep -E '^[[:space:]]*(state|pid|last exit code) =' || true
echo "Log: $LOG"

echo "==> building the menu-bar app"
BUILT="$("$REPO/scripts/build-app.sh")"

echo "==> staging $APP.tmp"
mkdir -p "$(dirname "$APP")"
rm -rf "$APP.tmp"
ditto "$BUILT" "$APP.tmp"

echo "==> rendering $APP_PLIST"
sed -e "s|@BIN@|$APP/Contents/MacOS/Yplayer|g" -e "s|@LOG@|$APP_LOG|g" \
    "$REPO/packaging/$APP_LABEL.plist.in" >"$APP_PLIST"

echo "==> stopping $DOMAIN/$APP_LABEL (if loaded)"
launchctl bootout "$DOMAIN/$APP_LABEL" 2>/dev/null || true
for _ in $(seq 50); do
    launchctl print "$DOMAIN/$APP_LABEL" >/dev/null 2>&1 || break
    sleep 0.1
done

echo "==> installing $APP"
rm -rf "$APP"
mv "$APP.tmp" "$APP"

echo "==> starting $DOMAIN/$APP_LABEL"
launchctl bootstrap "$DOMAIN" "$APP_PLIST"

echo "==> status"
launchctl print "$DOMAIN/$APP_LABEL" | grep -E '^[[:space:]]*(state|pid|last exit code) =' || true
echo "Log: $APP_LOG"
