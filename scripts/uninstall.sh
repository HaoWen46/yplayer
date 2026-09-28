#!/usr/bin/env bash
# Remove the com.yplayer.service LaunchAgent and ~/.local/bin/yplay.
# Keeps the cache, library DB, config.toml, and logs.
set -euo pipefail

LABEL="com.yplayer.service"
DOMAIN="gui/$UID"
BIN="$HOME/.local/bin/yplay"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"

echo "==> stopping $DOMAIN/$LABEL (if loaded)"
launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true

echo "==> removing $PLIST and $BIN"
rm -f "$PLIST" "$BIN"

echo "Kept: cache, library DB, config.toml, logs."
