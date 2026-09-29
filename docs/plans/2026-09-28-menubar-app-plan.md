# Sub-project 2 — menu-bar app: implementation plan

Spec: `docs/specs/2026-09-28-native-app-design.md` ("Menu-bar app (sub-project 2)", "Socket protocol (v1)", "Performance budget", "Error handling"). Branch: `native-app`. The service from sub-project 1 is the only backend; this plan adds a SwiftUI menu-bar app that talks to it over the Unix socket.

## Execution model

- Subagents implement tasks; the supervisor reviews every diff against this plan and the spec, runs all gates, merges, and verifies the UI with offscreen snapshots and live screenshots.
- Waves: W0 = S1+S2 (one agent, main checkout). W1 = S3 (agent A) ∥ S4 (agent B), worktrees. W2 = S5 (main checkout). W3 = S6 (agent C) ∥ S7 (agent D) ∥ S8 (agent E), worktrees. W4 = S9 (main checkout), then S10 (supervisor).
- Worktree agents: first run `git log --oneline -1`; if HEAD is not the base commit the supervisor names, `git reset --hard <base>` (the Agent tool creates worktrees from `main`).
- One commit per task, message `SP2 S<n>: …`, ending with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Rules for every task

- Test first where the task names tests (Swift Testing: `import Testing`, `@Test`, `#expect`); UI tasks verify with the snapshot renderer instead of unit tests.
- Gates before committing: `scripts/swift-test.sh` (all Swift tests), `swift build -c release --package-path apps/macos -Xswiftc -warnings-as-errors`, `swift format lint --strict --recursive apps/macos/Sources apps/macos/Tests`; if Rust or Python files changed, also the SP1 gates (`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `.venv/bin/ruff check yplayer/ tests/`, `.venv/bin/python -m pytest -q`).
- Swift 6 language mode with complete strict concurrency; no `@unchecked Sendable`, no `nonisolated(unsafe)` unless the task allows it and the commit message justifies it.
- No third-party Swift packages. Frameworks: SwiftUI, AppKit, Foundation, Network, MediaPlayer, ImageIO, UniformTypeIdentifiers.
- Tests and manual runs never touch the installed service (`~/Library/Application Support/yplayer/yplay.sock`) or `~/Music/yt-audio`: they start their own `yplay serve --dir <temp cache> --socket <path>` with `HOME=<temp>`, `YPLAY_NO_UPDATE=1`, `YPLAY_MPV_EXTRA_ARGS=--ao=null`, and a fake worker via `YPLAY_WORKER_CMD` when downloads are exercised. Socket paths must be shorter than 104 bytes: use `/tmp/ypsw-<pid>-<n>.sock` and remove them afterwards.
- The app never produces audio itself; audio always comes from the service's mpv.
- Touch only the files the task names; match the style of neighboring code.
- Return to the supervisor: files changed, tests added, gate results per gate, snapshot PNG paths (UI tasks), commit hash, deviations with reasons.

## Shared contracts

### Package layout (S1)

```
apps/macos/
  Package.swift                 swift-tools-version 6.2; platforms [.macOS(.v26)]; products: executable "Yplayer"
  .swift-format                 4-space indent, line length 100
  Packaging/Info.plist          CFBundleIdentifier com.yplayer.app, CFBundleName Yplayer, CFBundleExecutable Yplayer,
                                LSUIElement true, LSMinimumSystemVersion 26.0, CFBundleShortVersionString 0.1.0
  Sources/YplayerKit/           library: Protocol/, Client/, Store/ (no AppKit/SwiftUI imports)
  Sources/Yplayer/              executable: App/, Views/, Artwork/, NowPlaying/, Debug/
  Tests/YplayerKitTests/
scripts/swift-test.sh           exports YPLAY_BIN (see S3) then: cd apps/macos && swift test -Xswiftc -plugin-path -Xswiftc /Library/Developer/CommandLineTools/usr/lib/swift/host/plugins/testing "$@"
scripts/build-app.sh            swift build -c release; assemble build/Yplayer.app (Contents/MacOS/Yplayer, Contents/Info.plist); codesign --force --sign - ; prints the bundle path
```

- `swift test` needs the explicit `-plugin-path` above (Command Line Tools only; the Testing macros plugin is not found otherwise; XCTest is not available).

### YplayerKit protocol models (S2) — mirror of `crates/yplayer/src/protocol.rs`

- All wire keys are snake_case; Swift properties are camelCase with explicit `CodingKeys`.
- `TrackState: String { complete, downloading, failed }`; `LoopMode: String { none, single, all, shuffle }`; `PlayState: String { playing, paused, stopped }`; `DownloadPhase: String { fetching, downloading, done, failed, cancelled }`; `Severity: String { info, warn, error }`.
- `Track { id, title, uploader?, duration: Int?, webpageURL?, audioPath?, format?, fileSize: Int64?, addedAt: Int64?, lastPlayed: Int64?, state, thumbPath? }` (Identifiable, Equatable, Sendable).
- `Album { id: Int64, name, trackIDs: [String], createdAt: Int64, lastUsedAt: Int64? }`.
- `ContextRef { case album(Int64), library }` ↔ `{"album_id": N}` / `{"library": true}`.
- `PlayerState { state, trackID?, context: ContextRef?, position: Double, atMs: Int64, duration: Double?, volume: Double, loopMode }` (wire key `loop`).
- `AlbumRef { case id(Int64), name(String) }` ↔ `{"id": N}` / `{"name": "…"}`.
- `Command` enum with one case per wire command: `hello(protocol:)`, `subscribe`, `libraryGet`, `now`, `add(url:album:play:)`, `play(trackID:context:)`, `pause`, `resume`, `toggle`, `stop`, `next`, `prev`, `seek(position:)`, `volume(value:)`, `loop(mode:)`, `queuePlayNext(trackID:)`, `albumCreate(name:)`, `albumRename(albumID:name:)`, `albumDelete(albumID:)`, `albumAdd(albumID:trackID:)`, `albumRemove(albumID:trackID:)`, `albumReorder(albumID:trackIDs:)`, `trackDelete(trackID:toTrash:)`, `trackRename(trackID:title:)`, `trackRetry(trackID:)`, `rescan`, `lyrics(trackID:)`; `func line(id: UInt64) -> Data` produces exactly the wire JSON (field set and values) plus `\n`.
- `Event` enum decoded from a line with an `event` key: `player(PlayerState)`, `trackUpsert(Track)`, `trackRemoved(String)`, `albumUpsert(Album)`, `albumRemoved(Int64)`, `download(DownloadEvent)`, `toast(Toast)`, `resync`, `unknown(String)` (forward compatibility: unknown event names never throw).
- `DownloadEvent { trackID, phase, bytes: UInt64?, total: UInt64?, error: String? }`; `Toast { severity, message }`.
- `Response`: `{id, ok, result?, error?: {code, message}}`; `ServiceError: Error { code: String, message: String }`.
- Result types: `HelloResult { protocol: Int, serverVersion }`, `SubscribeResult { player: PlayerState, libraryVersion: UInt64 }`, `LibrarySnapshot { tracks: [Track], albums: [Album], libraryVersion: UInt64 }`, `NowResult { player, track: Track? }`, `AddResult { trackID, albumID: Int64, wasNew, wasInAlbum }`, `AlbumCreateResult { album }`, `LyricsResult { case lines(synced: Bool, [LyricLine]), missing }` with `LyricLine { tMs: Int64?, text }`, `EmptyResult`.
- `enum Line { case response(Response), event(Event) }` + `static func decode(_ line: Data) throws -> Line`.
- Golden fixtures: every command and event JSON string in `crates/yplayer/src/protocol.rs` tests is reproduced in a Swift test (encode commands, decode events) with identical expectations.

### ServiceClient (S3)

- `public actor ServiceClient` in YplayerKit/Client.
- `init(socketPath: String)`; `static func defaultSocketPath() -> String` (`YPLAY_SOCKET` env, else `~/Library/Application Support/yplayer/yplay.sock`).
- `nonisolated let updates: AsyncStream<ClientUpdate>` where `ClientUpdate { case connecting, connected(SubscribeResult), disconnected(retryIn: Duration), event(Event) }`.
- `func start()`: connect; on connect send `hello(protocol: 1)` (mismatch → disconnected, no retry storm: retry after 30 s) then `subscribe`, then yield `.connected`; read lines forever; on EOF/error yield `.disconnected` and reconnect with backoff 1 s, 2 s, 4 s … capped at 30 s, reset to 1 s after a successful connect.
- `func send<R: Decodable & Sendable>(_ cmd: Command, as: R.Type) async throws -> R` and `func send(_ cmd: Command) async throws`; ids from a counter; replies matched by id; 10 s timeout → `ServiceError(code: "timeout")`; not connected → `ServiceError(code: "not_connected")` immediately; `ok: false` → `ServiceError` with the server's code/message.
- Transport: Network.framework `NWConnection` to `NWEndpoint.unix(path:)`; if that proves unworkable, POSIX `socket(AF_UNIX)` + `DispatchIO` (record the choice); newline framing; lines > 1 MiB close the connection.
- No timers except the one-shot reconnect delay and per-request timeout tasks.

### Store (S4) — `@MainActor @Observable public final class LibraryStore` in YplayerKit/Store

- State: `tracks: [String: Track]`, `libraryOrder: [String]` (added_at desc, then title via `localizedStandardCompare`), `albums: [Album]` (name, `localizedStandardCompare`), `player: PlayerState?`, `downloads: [String: DownloadProgress]` (`phase`, `fraction: Double?`), `connection: ConnectionStatus` (`connecting | connected | disconnected(retryAt: Date)`), `toasts: [ToastItem]` (`id: UUID`, severity, message), `libraryVersion: UInt64`.
- `func apply(_ update: ClientUpdate)`: connection and events; `player` events replace `player` only when any field other than `atMs`/`position` differs or the position jumps by more than 1 s from the extrapolated clock (coalesces the service's 3–4 duplicate events per change); `track.upsert` inserts/updates and repositions in `libraryOrder`; `track.removed` removes from tracks, order, albums, downloads; `album.upsert`/`album.removed`; `download` updates progress (removed on done/failed/cancelled); `toast` appends; `resync` sets `needsResync = true`.
- `func load(_ snapshot: LibrarySnapshot)` replaces everything and clears `needsResync`.
- Derived: `currentTrack: Track?`, `func tracks(in album: Album) -> [Track]`, `func search(_ query: String) -> [Track]` (empty query → []), `func album(id:) -> Album?`.
- `SearchNormalizer.fold(_:)`: `folding(options: [.caseInsensitive, .diacriticInsensitive, .widthInsensitive])` then hiragana→katakana (`applyingTransform(.hiraganaToKatakana, reverse: false)`); the store caches folded title+uploader per track id (recomputed on upsert) so a keystroke over 5,000 tracks is a substring scan only.
- `PositionClock.position(_ p: PlayerState, now: Date) -> Double`: playing → `p.position + (now − atMs)`, clamped to `[0, duration]`; otherwise `p.position`.
- `LyricsTimeline.index(_ lines: [LyricLine], at seconds: Double) -> Int?` (last line with `tMs <= t`).

### App components (S5–S8) — Sources/Yplayer

- `main.swift` / `AppDelegate`: `NSApplication.shared` with `.accessory` activation policy; parses flags; creates `AppModel`; installs `StatusItemController`.
- `AppModel` (`@MainActor @Observable`, S5): owns `ServiceClient` and `LibraryStore`; a task consumes `client.updates` into the store and calls `libraryGet` after every `.connected` and whenever `store.needsResync`; intents (all `async`, errors become `store` toasts, never alerts): `play(trackID:context:)`, `toggle()`, `next()`, `prev()`, `seek(to:)`, `setVolume(_:)`, `cycleLoop()`, `playNext(_:)`, `createAlbum(_:)`, `renameAlbum(_:_:)`, `deleteAlbum(_:)`, `addToAlbum(_:_:)`, `removeFromAlbum(_:_:)`, `reorderAlbum(_:_:)`, `deleteTrack(_:)`, `retry(_:)`, `renameTrack(_:_:)`, `lyrics(for:) async -> LyricsResult?`, `showInFinder(_:)`, `copyLink(_:)`.
- `StatusItemController` (S5): `NSStatusItem` with SF Symbol `music.note` (not playing) / `waveform` (playing), swapped only when `player.state` changes; left click toggles an `NSPopover` (`.transient`, `animates = true`); its `contentViewController` is an `NSHostingController(rootView: PopoverView(model:))` created on show and set to `nil` in `popoverDidClose` so nothing renders or ticks while closed; on show `NSApp.activate()` so keyboard shortcuts work.
- `PopoverView` (S5): 360×560 pt; `VStack { ConnectionBanner; NowPlayingCard; LibraryTabs }` + overlays `ConfirmOverlay` and `ToastBanner`; `ConfirmRequest` state lives in `AppModel` (`@Observable var confirm: ConfirmRequest?`). S5 ships `NowPlayingCard` and `LibraryTabs` as minimal placeholders in their own files, which S6 and S7 replace.
- `NowPlayingCard` (S6): artwork 64 pt (corner radius 8), title `.headline` 1 line, uploader `.subheadline` secondary 1 line; scrubber `Slider` bound to a local drag value, commits `seek` on release, driven by `TimelineView(.periodic(from: .now, by: 1))` only while `player.state == .playing`; elapsed/remaining in `.caption.monospacedDigit()`; transport row `backward.fill` · `play.fill`/`pause.fill` (larger) · `forward.fill` with `.buttonStyle(.glass)`; loop button cycles none→all→single→shuffle with symbols `repeat` (dimmed for none), `repeat`, `repeat.1`, `shuffle`; volume `Slider` with `speaker.wave.2.fill`, commits on release; lyrics toggle `quote.bubble`; `⋯` menu (`ellipsis.circle`): Remove from Album… (album context only), Delete from Library…, Show in Finder, Copy YouTube Link; card background `.glassEffect(.regular, in: .rect(cornerRadius: 16))`; nothing playing → placeholder art + "Not playing".
- `LyricsView` (S6): shown in place of `LibraryTabs` when toggled; fetches via `model.lyrics(for:)` when the current track changes; synced → list with the active line highlighted (`LyricsTimeline`) and auto-scrolled, ticking with `TimelineView(.periodic(by: 0.5))` only while visible and playing; unsynced → plain text; missing → "No lyrics found"; error → "Lyrics unavailable".
- `ArtworkLoader` (S6): `actor`; `func image(for path: String, pointSize: CGFloat, scale: CGFloat) async -> CGImage?` using `CGImageSourceCreateThumbnailAtIndex` with `kCGImageSourceThumbnailMaxPixelSize = pointSize × scale`; `NSCache` keyed by path+size; missing file → nil (placeholder `music.note` on a tinted rounded rect).
- `LibraryTabs` (S7): segmented `Picker` Albums | Songs | Search; ⌘F selects Search and focuses its field.
- Rows (S7): `TrackRow` 36 pt artwork, title (1 line), secondary line `uploader · m:ss`; playing row shows `waveform` in the accent color (static, no animation); downloading → small determinate `ProgressView(value:)` (indeterminate while fetching); failed → `exclamationmark.triangle` in orange; double-click or Return plays; single click selects.
- `AlbumsView` (S7): list of albums (`name`, `n songs`), + button → inline name field → `album.create`; double-click name → inline rename; context menu Delete Album… (confirm: "Delete album “X”? Its songs stay in your library."); click → `AlbumDetailView`.
- `AlbumDetailView` (S7): back button, album title, track list in album order with `.onMove` reorder → `album.reorder`; swipe-left (trackpad) and ⌫ → confirm "Remove “X” from Y? The song stays in your library."; playing a row uses context `album(id)`.
- `SongsView` (S7): `libraryOrder`; swipe-left and ⌫ → confirm "Delete “X” from your library? The <size> file moves to the Trash." (destructive); ⌘⌫ same everywhere; playing uses context `library`.
- `SearchView` (S7): `TextField` "Search your library"; results from `store.search`; same row actions as Songs.
- Context menu on any track row (S7): Play Next, Add to Album ▸ (albums + New Album…), Remove from Album… (album view), Delete from Library…, Show in Finder, Copy YouTube Link, Retry (failed only), Rename….
- `ConfirmOverlay` (S5): dimmed backdrop + centered card (title, message, Cancel + destructive button with `role: .destructive`); Esc cancels; no default button; used instead of NSAlert so the transient popover never loses key status.
- `ToastBanner` (S5): bottom banner, auto-dismiss after 4 s (errors 6 s) via a one-shot `Task.sleep` while visible.
- `ConnectionBanner` (S5): when disconnected: "Can't reach the yplay service — retrying in Ns" with a Retry Now button (calls `client.start()` again).
- Keyboard (S7 for list keys, S5 for global): Space toggles play/pause (not when a text field is focused), ⌘F search, ⌫ remove/delete (context-dependent, confirmed), ⌘⌫ delete from library (confirmed), Esc closes the confirm overlay or else the popover.
- `NowPlayingController` (S8): `MPNowPlayingInfoCenter.default()` title/artist/duration/elapsed/rate/artwork + `playbackState`, updated only when `store.player` or the current track changes (macOS extrapolates elapsed time); `MPRemoteCommandCenter` play/pause/toggle/next/previous/changePlaybackPosition → `AppModel` intents.

### Debug flags (S5; extended by S6/S7)

- `--snapshot-dir <dir>`: no socket; builds a `LibraryStore` from `DebugFixtures` (8 tracks with Zutomayo-style CJK titles such as `ずっと真夜中でいいのに。『秒針を噛む』MV`, one Korean title, one long Latin title, 2 albums, one downloading at 40 %, one failed, player playing at 1:02 / 3:51 with lyrics) and writes PNGs at 2× with `ImageRenderer` for: `popover-albums`, `popover-songs`, `popover-search` (query "ham"), `popover-album-detail`, `popover-lyrics`, `popover-confirm-delete`, `popover-disconnected`, `popover-empty`; then exits 0. Each later UI task adds its own states to this list.
- `--open-popover`: opens the popover 1 s after launch (for live screenshots).

### Performance rules (app)

- Popover closed: no SwiftUI view tree exists (hosting controller released); no timers; only the socket read and event application run.
- Popover open: at most one scrubber redraw per second; lyrics at most two redraws per second, only while visible and playing.
- Budget (spec): popover closed & nothing playing → 0 idle wakeups/s, < 40 MB; playing & closed → ~0 wakeups/s; popover open & playing → ≤ 1 redraw/s.

## Tasks

### S1 — Package scaffold, scripts, minimal app (W0, main checkout, with S2)

- Files: everything under "Package layout" except the protocol/client/store/view contents; `apps/macos/Sources/Yplayer/App/main.swift`, `AppDelegate.swift` with a status item (`music.note`) whose click shows an empty popover with the text "yplayer"; `apps/macos/Tests/YplayerKitTests/SmokeTests.swift` (one trivial test); `.gitignore` entry `apps/macos/.build/` and `build/`; `justfile` recipes `swift-test`, `app` (build-app.sh).
- Acceptance: `scripts/swift-test.sh` passes; `scripts/build-app.sh` produces `build/Yplayer.app`; launching `build/Yplayer.app/Contents/MacOS/Yplayer --open-popover` runs with no Dock icon (`LSUIElement`) — report `ps` and that the process stays up for 5 s; kill it.
- Commit: `SP2 S1: SwiftPM app scaffold, test/build scripts, minimal status item`.

### S2 — Protocol models and codec (W0, after S1)

- Files: `Sources/YplayerKit/Protocol/*.swift`, `Tests/YplayerKitTests/ProtocolTests.swift`.
- Tests first: encode every command and compare parsed JSON objects to the Rust golden strings (key sets and values equal); decode every golden event string; decode responses (ok with result, error with code/message); unknown event name → `.unknown(name)`; `Track` with all-null optionals decodes; CJK titles round-trip; `PlayerState` uses key `loop`; `ContextRef`/`AlbumRef` shapes.
- Commit: `SP2 S2: protocol v1 models and line codec in YplayerKit`.

### S3 — ServiceClient (W1, agent A, worktree)

- Files: `Sources/YplayerKit/Client/*.swift`, `Tests/YplayerKitTests/ClientTests.swift`.
- Tests (integration, against the real `yplay` binary whose path is in env `YPLAY_BIN`; `scripts/swift-test.sh` exports `YPLAY_BIN=<repo>/target/release/yplay` when that file exists; tests are skipped with a clear message when `YPLAY_BIN` is unset; in a worktree run `cargo build --release` first): start `yplay serve` as described in the rules; `connected` arrives with a `SubscribeResult`; `libraryGet` returns an empty library; `albumCreate` returns an album and an `album.upsert` event arrives on `updates`; a server error maps to `ServiceError` with the server code (`album.delete` of a missing id → `not_found`); killing the service yields `.disconnected` and restarting it yields `.connected` again within the backoff; a request while disconnected fails fast with `not_connected`; the client never leaks the socket file (the service owns it).
- Commit: `SP2 S3: ServiceClient over the Unix socket with reconnect`.

### S4 — Store, clock, search (W1, agent B, worktree)

- Files: `Sources/YplayerKit/Store/*.swift`, `Tests/YplayerKitTests/StoreTests.swift`.
- Tests first: load snapshot orders; upsert new/updated track repositions; removed track disappears from albums and downloads; duplicate player events are coalesced (feed the service's typical 4-event burst; observers see one change) while a real seek (> 1 s jump) is applied; download progress fraction; toast append; resync flag; `PositionClock` clamps; `SearchNormalizer` matches `ハム` for query `はむ`, `ＺＵＴＯＭＡＹＯ` for `zutomayo`, `Beyoncé` for `beyonce`; search over 5,000 synthetic tracks returns in < 5 ms (measure with `ContinuousClock`, assert < 20 ms to avoid flakiness, report the number).
- Commit: `SP2 S4: LibraryStore reducer, position clock, kana-insensitive search`.

### S5 — App shell (W2, main checkout)

- Files: `App/AppModel.swift`, `App/StatusItemController.swift`, `App/main.swift`/`AppDelegate.swift` (wire-up and flags), `Views/PopoverView.swift`, `Views/NowPlayingCard.swift` (placeholder), `Views/LibraryTabs.swift` (placeholder), `Views/ConfirmOverlay.swift`, `Views/ToastBanner.swift`, `Views/ConnectionBanner.swift`, `Debug/DebugFixtures.swift`, `Debug/SnapshotRenderer.swift`.
- Acceptance: gates pass; `--snapshot-dir` writes `popover-disconnected.png`, `popover-confirm-delete.png`, `popover-empty.png`; live: with a temp `yplay serve` (rules) and `YPLAY_SOCKET` pointing at it, `--open-popover` shows the connected empty state (supervisor screenshots it); the status item glyph switches to `waveform` when the temp service plays a fixture (use `yplay add` with the fake worker from `crates/yplayer/tests/cli_e2e.rs` style).
- Commit: `SP2 S5: status item, lazy popover, app model, banners and confirm overlay`.

### S6 — Now-playing card, artwork, lyrics (W3, agent C, worktree)

- Files: `Views/NowPlayingCard.swift`, `Views/TransportControls.swift`, `Views/LyricsView.swift`, `Artwork/ArtworkLoader.swift`, `Debug/DebugFixtures.swift` (add states only), `Debug/SnapshotRenderer.swift` (add states only).
- Acceptance: gates; snapshots `card-playing`, `card-paused`, `card-nothing`, `popover-lyrics`, `lyrics-missing`; scrubber and lyrics timers exist only while visible and playing (state in the commit message how this is guaranteed).
- Commit: `SP2 S6: now-playing card with glass controls, artwork loader, synced lyrics`.

### S7 — Library lists and interactions (W3, agent D, worktree)

- Files: `Views/LibraryTabs.swift`, `Views/AlbumsView.swift`, `Views/AlbumDetailView.swift`, `Views/SongsView.swift`, `Views/SearchView.swift`, `Views/TrackRow.swift`, `Views/TrackContextMenu.swift`, `Debug/DebugFixtures.swift` and `Debug/SnapshotRenderer.swift` (add states only).
- Acceptance: gates; snapshots `popover-albums`, `popover-album-detail`, `popover-songs` (with downloading and failed rows), `popover-search`, `row-context-menu` is not renderable offscreen — instead list the menu items in the commit message; every destructive action routes through `ConfirmOverlay` with the spec's wording.
- Commit: `SP2 S7: albums, songs, search, context menus, reorder, confirmed removal`.

### S8 — Now Playing and media keys (W3, agent E, worktree)

- Files: `NowPlaying/NowPlayingController.swift`, one wire-up line in `App/AppModel.swift` (or `AppDelegate.swift`) marked for the supervisor to merge; `crates/yplayer/src/player/mpv.rs` (add `--media-controls=no` next to `--input-media-keys=no`, with the existing comment style).
- Also: `Sources/YplayerKit/Store/NowPlayingInfo.swift` — a pure `NowPlayingInfo.make(player: PlayerState?, track: Track?) -> NowPlayingInfo?` (title, artist, duration, elapsed, rate 1/0, playing flag, artwork path) — and `Tests/YplayerKitTests/NowPlayingInfoTests.swift`; the app's `NowPlayingController` only translates that struct into MediaPlayer calls (the app target has no test target).
- Acceptance: gates (Swift and Rust); the pure mapping is tested (playing, paused, nothing playing, missing duration, CJK title).
- Commit: `SP2 S8: Now Playing and media keys via MediaPlayer; mpv media controls off`.

### S9 — App install, login start, perf rows (W4, main checkout)

- Files: `packaging/com.yplayer.app.plist.in`, `scripts/install.sh` (build the app with `scripts/build-app.sh`, install to `~/Applications/Yplayer.app`, render and bootstrap `com.yplayer.app` with `RunAtLoad` true, `KeepAlive` `{SuccessfulExit: false}`, `LimitLoadToSessionType` Aqua, `ProgramArguments` = the app executable), `scripts/uninstall.sh` (remove app agent and bundle), `scripts/perf-budget.sh` (add `app-closed` and `app-open` states measuring the `Yplayer` process: idle wakeups/s ≤ 0.2 and memory < 40 MB when closed; ≤ 1.5 wakeups/s when open and playing), `README.md` (menu-bar app section).
- Acceptance: `bash -n` + shellcheck; the supervisor runs the real install after user approval.
- Commit: `SP2 S9: install the menu-bar app as a login agent; app perf rows`.

### S10 — Verification (supervisor)

- Review every snapshot PNG; live screenshots of the popover with the installed service; perf rows `app-closed` / `app-open`; user-assisted checks: media keys and Control Center show the current song, keyboard shortcuts, swipe, drag reorder, delete-to-Trash confirmation, reconnect after `launchctl kickstart -k gui/$UID/com.yplayer.service`.
- Record results in the spec; update memory.
