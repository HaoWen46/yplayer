#!/usr/bin/env bash
# Remove the com.yplayer.app and com.yplayer.service LaunchAgents,
# ~/Applications/Yplayer.app, ~/.local/bin/yplay, and a release install's
# worker venv. Keeps your music folder (songs and library DB), config.toml,
# and logs. YPLAYER_NO_LAUNCHD=1 leaves launchd alone (tests).
set -euo pipefail

LABEL="com.yplayer.service"
DOMAIN="gui/$UID"
BIN="$HOME/.local/bin/yplay"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
APP_LABEL="com.yplayer.app"
APP="$HOME/Applications/Yplayer.app"
APP_PLIST="$HOME/Library/LaunchAgents/$APP_LABEL.plist"
STATE="$HOME/Library/Application Support/yplayer"

stop_agent() {
    [ -z "${YPLAYER_NO_LAUNCHD:-}" ] || return 0
    echo "==> stopping $DOMAIN/$1 (if loaded)"
    launchctl bootout "$DOMAIN/$1" 2>/dev/null || true
}

stop_agent "$APP_LABEL"

echo "==> removing $APP_PLIST and $APP"
rm -f "$APP_PLIST"
rm -rf "$APP"

stop_agent "$LABEL"

echo "==> removing $PLIST and $BIN"
rm -f "$PLIST" "$BIN"

# Installed by a release package (scripts/get.sh); absent for checkouts.
echo "==> removing $STATE/venv"
rm -rf "$STATE/venv"
rm -f "$STATE/uninstall.sh"

echo "Kept: your music folder (songs and library DB), config.toml, logs."
