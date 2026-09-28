#!/usr/bin/env bash
# Build yplay and install `yplay serve` as the LaunchAgent com.yplayer.service.
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
        sqlite3 "$DB" ".backup '$BAK'"
    else
        cp -p "$DB" "$BAK"
    fi
    echo "==> backed up $DB -> $BAK"
fi

echo "==> starting $DOMAIN/$LABEL"
launchctl bootstrap "$DOMAIN" "$PLIST"

echo "==> status"
launchctl print "$DOMAIN/$LABEL" | grep -E '^[[:space:]]*(state|pid|last exit code) =' || true
echo "Log: $LOG"
