#!/usr/bin/env bash
set -euo pipefail
# run CLI in dev mode from source (no install needed)
PYTHONPATH=$(cd "$(dirname "$0")/.." && pwd) python3 -m yplayer.cli "$@"
