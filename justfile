# yplayer developer tasks. Run `just <task>`; `just` alone lists tasks.

default:
    @just --list

# The same gates CI runs, offline.
check: check-rust check-python

check-rust:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

check-python:
    .venv/bin/ruff check yplayer/
    .venv/bin/python -m pytest -q

# Auto-fix what can be fixed.
fix:
    cargo fmt --all
    ruff check yplayer/ --fix

# Build, install ~/.local/bin/yplay, and (re)start the com.yplayer.service LaunchAgent.
install:
    scripts/install.sh

# Remove the LaunchAgent and ~/.local/bin/yplay (keeps cache, DB, config).
uninstall:
    scripts/uninstall.sh

# Check the running service against the performance budget (STATE: idle | playing).
perf STATE:
    scripts/perf-budget.sh {{STATE}}

# Ignored tests (real mpv, network, CLI end-to-end); run before releases.
e2e:
    cargo test --workspace -- --ignored
