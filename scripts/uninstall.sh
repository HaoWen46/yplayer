#!/usr/bin/env bash
# Remove the com.yplayer.app and com.yplayer.service LaunchAgents,
# ~/Applications/Yplayer.app, and ~/.local/bin/yplay.
# Keeps the cache, library DB, config.toml, and logs.
set -euo pipefail

LABEL="com.yplayer.service"
DOMAIN="gui/$UID"
BIN="$HOME/.local/bin/yplay"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
APP_LABEL="com.yplayer.app"
APP="$HOME/Applications/Yplayer.app"
APP_PLIST="$HOME/Library/LaunchAgents/$APP_LABEL.plist"

echo "==> stopping $DOMAIN/$APP_LABEL (if loaded)"
launchctl bootout "$DOMAIN/$APP_LABEL" 2>/dev/null || true

echo "==> removing $APP_PLIST and $APP"
rm -f "$APP_PLIST"
rm -rf "$APP"

echo "==> stopping $DOMAIN/$LABEL (if loaded)"
launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true

echo "==> removing $PLIST and $BIN"
rm -f "$PLIST" "$BIN"

echo "Kept: cache, library DB, config.toml, logs."
