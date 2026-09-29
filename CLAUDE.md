# yplayer — agent notes

- Layout: Rust service + CLI in `crates/yplayer`, SwiftUI menu-bar app + drop orb in `apps/macos`, Python yt-dlp worker in `yplayer/` (tests in `tests/`); design and verification results in `docs/specs/2026-09-28-native-app-design.md`.
- Gates before every commit: `just check` (Rust fmt, clippy -D warnings, tests; ruff + pytest; swift format lint --strict, warnings-as-errors release build, Swift tests); `just e2e` runs the ignored tests (real mpv, network, CLI end-to-end).
- Python is `uv`-only; the worker venv is `.venv` (`uv pip install --python .venv/bin/python -e .`).
- Tests never touch `~/Music/yt-audio`, the installed service socket, launchd or the running apps: use temp dirs and `YPLAY_NO_UPDATE`; sockets go under `/tmp` because macOS socket paths must stay under 104 bytes.
- Command Line Tools only (no Xcode): run Swift tests via `scripts/swift-test.sh` (it passes the Testing plugin path).
- SwiftUI macros are unavailable with the Command Line Tools: views use `@ViewState` (`typealias ViewState = SwiftUI.State`), never `@State`, `@Entry` or `#Preview`; Observation macros work.
- `ImageRenderer` cannot draw Liquid Glass; UI snapshots use the app's `--snapshot-dir` flag (offscreen window + `screencapture -l`).
- The service is the only writer of the SQLite DB, the cache folder and mpv; the app and CLI go through the socket protocol (`crates/yplayer/src/protocol.rs`, mirrored in `apps/macos/Sources/YplayerKit/Protocol`).
- Every folder delete goes through `library::safe_fs::remove_track_dir` (direct child of the cache, `[id8]` name suffix, never a symlink; automatic cleanups refuse folders holding user files).
- mpv needs `--audio-format=float` on macOS 27, or it falls back to avfoundation with a ~4 s buffer.
- yt-dlp updates belong to the service (daily check, exact release tag, binary-only, at least a day old); never update on the request path.
- `scripts/install.sh` and `scripts/uninstall.sh` change the user's Mac (LaunchAgents, `~/Applications`): run them only when asked; to test them use a temp `HOME` plus `YPLAYER_NO_LAUNCHD=1` (launchd labels are global).
- Releases: one version in `crates/yplayer/Cargo.toml`, `pyproject.toml`, `yplayer/__init__.py`, `apps/macos/Packaging/Info.plist` (checked by `scripts/package-release.sh`); tag `vX.Y.Z` on main → `.github/workflows/release.yml` publishes `yplayer-macos-arm64.tar.gz` (+ `.sha256`) that `scripts/get.sh` installs (arm64 only).
- A release install sets `worker_python` to `~/Library/Application Support/yplayer/venv/bin/python3`; a later checkout `scripts/install.sh` keeps it, so delete that line from `config.toml` first when testing worker changes from a checkout.
- CI runners use Xcode's Swift (older than local CLT, 6.3.3 vs 6.4): passing a MainActor method directly as a closure (e.g. `Binding(set: method)`) crashed it; wrap it in `{ method($0) }`.
- Test with CJK titles; the real library is mostly Japanese music.
- `README.md` and `README.zh-TW.md` (Traditional Chinese, Taiwan terms) are kept in sync: mirror every README change in both; UI labels stay in English because the app is English.
