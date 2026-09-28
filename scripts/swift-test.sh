#!/usr/bin/env bash
# Run the menu-bar app's Swift tests. Command Line Tools only: the Testing
# macros plugin is not found without the explicit -plugin-path.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ -f "$REPO/target/release/yplay" ]; then
    export YPLAY_BIN="$REPO/target/release/yplay"
fi

cd "$REPO/apps/macos"
swift test -Xswiftc -plugin-path -Xswiftc /Library/Developer/CommandLineTools/usr/lib/swift/host/plugins/testing "$@"
