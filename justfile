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
    ruff check yplayer/
    pytest -q

# Auto-fix what can be fixed.
fix:
    cargo fmt --all
    ruff check yplayer/ --fix

# Real-worker end-to-end check (network); run before releases.
e2e:
    cargo test --workspace -- --ignored
