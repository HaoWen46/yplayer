# yplayer developer tasks. Run `just <task>`; `just` alone lists tasks.

default:
    @just --list

# The same gates CI runs, offline.
check: check-rust check-python check-swift

check-rust:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

check-python:
    .venv/bin/ruff check yplayer/ tests/
    .venv/bin/python -m pytest -q

check-swift:
    swift format lint --strict --recursive apps/macos/Sources apps/macos/Tests
    swift build -c release --package-path apps/macos -Xswiftc -warnings-as-errors
    scripts/swift-test.sh

# Auto-fix what can be fixed.
fix:
    cargo fmt --all
    .venv/bin/ruff check yplayer/ tests/ --fix
    swift format --in-place --recursive apps/macos/Sources apps/macos/Tests

# Build and install the service and the menu-bar app, then (re)start both LaunchAgents.
install:
    scripts/install.sh

# Stop both LaunchAgents; remove ~/.local/bin/yplay, the app and the plists (keeps cache, DB, config).
uninstall:
    scripts/uninstall.sh

# Check the running service/app against the performance budget (STATE: idle | playing | app-closed | app-open).
perf STATE:
    scripts/perf-budget.sh {{STATE}}

# Ignored tests (real mpv, network, CLI end-to-end); run before releases.
e2e:
    cargo test --workspace -- --ignored

# Menu-bar app Swift tests.
swift-test:
    scripts/swift-test.sh

# Build and ad-hoc sign build/Yplayer.app.
app:
    scripts/build-app.sh

# Redraw the app icon (apps/macos/Packaging/AppIcon.icns) after editing scripts/make-icon.swift.
icon:
    swift scripts/make-icon.swift apps/macos/Packaging/AppIcon.icns
