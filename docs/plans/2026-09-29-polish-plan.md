# Polish plan: loudness leveling, Up Next, settings window, app icon

Branch `polish` (base: this plan's commit). Tracks L, Q, S run in parallel worktrees; track I (app icon) is done by the supervisor. Keep edits inside the files each track owns; shared files (`protocol.rs`, `core.rs`, `engine.rs`, Swift `Protocol/*`, `AppModel.swift`, `PopoverView.swift`, `LibraryStore.swift`) get additive, local edits only so merges stay small.

## Rules (all tracks)

- Test first for every behavior listed under a track's Tests; tests never touch `~/Music/yt-audio`, the installed service socket, launchd or the running apps; sockets under `/tmp` (< 104 bytes); set `YPLAY_NO_UPDATE`.
- Gates: Rust `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; Swift `scripts/swift-test.sh`, `swift build -c release --package-path apps/macos -Xswiftc -warnings-as-errors`, `swift format lint --strict --recursive apps/macos/Sources apps/macos/Tests` — run the ones for languages you touched.
- Command Line Tools only: SwiftUI views use `@ViewState`, never `@State`/`@Entry`/`#Preview`.
- Performance: no new timers or polling while idle; events only when their payload changed; windows and views released when closed.
- One commit per track: `Polish <track>: …`, ending with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Shared protocol contract (v1, additive)

- `settings.get` → `Settings` = `{"level_loudness": bool, "loudness_available": bool, "api_key": string|null, "music_folder": string}`.
- `settings.set` with optional `level_loudness: bool`, `api_key: string|null` (absent = unchanged; null or blank after trim = remove; > 200 chars or containing whitespace/control characters = `bad_request`) → the new `Settings`; then event `{"event": "settings", <Settings fields>}` to subscribers.
- `library.move` with `to: string` → `{"restarting": true}`, then the service shuts down cleanly and exits 0 about 300 ms after the reply (launchd `KeepAlive` restarts it; the move happens at the next startup). Errors: `bad_request` "The new location must be a full path." (not absolute), "That folder's parent doesn't exist." , "Something already exists at that location.", "The new location can't be inside your music folder.", "Choose a folder on the same disk as your music folder." (parent's device differs from the music folder's); `conflict` "Wait for downloads to finish before moving your music folder." (any download in flight).
- `queue.get` → `QueueState` = `{"next": [track_id], "upcoming": [track_id], "more": bool, "context": ContextRef|null}`: `next` is the play-next FIFO (all entries); `upcoming` is the context tracks that play after `next`, in real play order (Shuffle and All continue into the following cycle, `Single` is ignored as `next` does), at most 100; `more` is true when `upcoming` was cut at 100. Event `{"event": "queue", <QueueState fields>}` whenever the state differs from the last one emitted.
- `queue.remove` `{section: "next"|"upcoming", index, track_id}`; `queue.move` `{from, to, track_id}` (section `next` only; `to` is the final index); `queue.clear` (empties `next`); `queue.jump` `{section, index, track_id}` plays that entry now: for `next[i]` drop `next[0..=i]`; for `upcoming[i]` empty `next` and skip `upcoming[0..i]`. Each returns `{}`; an index out of range → `not_found`; `track_id` not at `index` → `conflict` "Up Next changed; try again.".

## Track L — loudness leveling, settings, music folder move (Rust service)

Owns: new `crates/yplayer/src/loudness.rs`, `lib.rs`, `config.rs`, `library/db.rs`, `player/engine.rs` (gain only), `service/core.rs` (resolver, measurement scheduling, settings/move handlers), `service/mod.rs` (startup), `protocol.rs` (settings/move commands and event), `crates/yplayer/Cargo.toml` (`toml_edit`), `README.md` (config table + a "Loudness" paragraph).

- L1: `loudness::measure(ffmpeg, path)` runs `/usr/sbin/taskpolicy -b <ffmpeg> -hide_banner -nostats -nostdin -i <path> -map 0:a:0 -af ebur128=peak=sample:framelog=quiet -f null -` (kill after 10 min) and parses the final summary: `I: <x> LUFS` and, under `Sample peak:`, `Peak: <y> dBFS`; I < -60 (silence) or unparsable → failure. Measured cost: 0.46 s for a 4.5 min song (1.1 s at background priority); true peak costs 4× and is not used. ffmpeg is found with `which::which("ffmpeg")` at startup (the LaunchAgent PATH includes /opt/homebrew/bin); none → `loudness_available = false`, no measurements.
- L2: DB schema v2 (`PRAGMA user_version = 2`): `tracks.loudness REAL`, `tracks.sample_peak REAL`, `tracks.loudness_checked INTEGER NOT NULL DEFAULT 0`; every statement that changes `audio_path` resets all three.
- L3: one measurement at a time, off the core loop, results back as an internal message. Order: tracks that just finished downloading, then the playing or preloaded track when unmeasured, then a backfill of every complete unmeasured track after the startup reconcile (`last_played DESC, added_at DESC`). Never measure a track that is still downloading. Failure stores `loudness_checked = 1` with NULL values. Nothing runs when the queue is empty.
- L4: gain (dB) = `TARGET − I` with TARGET = −14 LUFS, then `min(gain, −1.0 − peak)`, clamped to [−20, +10]. Unmeasured or failed tracks use the median gain of measured tracks (kept in a sorted in-memory list, updated per result; 0 when there are none). Leveling off → 0 for every track.
- L5: `TrackResolver::gain_db(track_id) -> f64`. `load` and `preload` always pass per-file options through loadfile's 4th argument with index −1: `volume-gain=<g, 2 decimals>`, plus `start=<pos>` when resuming (comma-separated). Verified on mpv 0.41: per-file `volume-gain` applies to the appended file when it starts and `set_property volume-gain` changes it live. `refresh_preload` compares (id, path, gain) so a new measurement re-preloads the next track. A measurement for the playing track never changes it mid-song. Toggling `level_loudness` sets `volume-gain` live on the playing track and re-preloads.
- L6: settings persistence in `config.toml` (`level_loudness`, default true; `api_key`; `cache_dir`) through `toml_edit`, keeping comments and unknown keys; written to a temp file (mode 0600) and renamed. The in-memory `Config` updates too.
- L7: `library.move` per the contract; it writes `<state dir>/pending-move.json` `{from, to}`. Startup, before opening the DB: if that file exists, delete it first (no retry loop), and if `from` is the current cache dir, `from` exists and `to` does not, `rename(from, to)`; on success set the cache dir to `to`, write `cache_dir` to `config.toml`, and after opening the DB rewrite `audio_path`/`thumb_path` prefixes `from/` → `to/` in one transaction, then an info toast "Moved your music folder to <to>."; on failure a warn toast "Couldn't move your music folder: <error>." and keep `from`.
- L8: `settings.get/set` and the `settings` event per the contract.
- Tests: ebur128 parsing (normal, sample-peak block, silence −70, garbage); gain math (target, headroom, clamps, median fallback, leveling off); migration v1→v2 and reset on `audio_path` change; engine loadfile args for load, resume and preload, re-preload on gain change, live gain on toggle; scheduler order and single-flight with a fake measurer; config writer keeps comments and unknown keys; settings validation and event; `library.move` validation (inject the device check) and the startup move with DB rewrite in temp dirs; an `#[ignore]` real-ffmpeg test on `tests/fixtures/tone.opus` (expect I ≈ −21.8 LUFS).

## Track Q — Up Next (Rust queue + Swift view)

Owns: `player/queue.rs`, `player/engine.rs` (queue-edit methods only), `protocol.rs` (queue commands and event), `service/core.rs` (queue handlers + change-only emission), Swift `YplayerKit/Protocol/*`, `YplayerKit/Store/LibraryStore.swift`, new `Yplayer/Views/UpNextView.swift`, `Yplayer/Views/NowPlayingCard.swift`, `Yplayer/Views/PopoverView.swift`, `Yplayer/App/AppModel.swift`, `Yplayer/Debug/SnapshotRenderer.swift` + `DebugFixtures.swift`, tests.

- Q1: `Queue::snapshot(limit)` without mutation; edits `remove_next(i)`, `remove_upcoming(i)`, `move_next(from, to)`, `clear_next()`, `jump(section, i)`; engine wrappers re-preload after each edit.
- Q2: core handlers per the contract; after every request, mpv event and internal message, compute the snapshot and emit `queue` only if it differs from the last emitted one.
- Q3: Swift `QueueState` model, `.queue` event, the five commands; `LibraryStore.queue`; `AppModel` fetches `queue.get` on connect (with `library.get`) and has intents for remove/move/clear/jump.
- Q4: `UpNextView`: an Up Next button (`list.bullet`) next to the lyrics button toggles `model.showsQueue` (mutually exclusive with lyrics); `PopoverView` shows it in the library's place. Sections "Playing Next" (Clear button; drag to reorder) and "Up Next from <album name | Library>"; rows show artwork, title and uploader like `TrackRow`, failed tracks dimmed; double-click or Return plays the row (`queue.jump`); context menu Play Now / Remove from Up Next; swipe left and ⌫ remove (no confirmation — nothing is deleted); "Repeating this song" note in `Single`; "and more…" footer when `more`; empty state "Nothing is up next."; follows the popover's Liquid Glass style.
- Q5: snapshot state `upnext` in `SnapshotRenderer` with a fixture of 3 play-next and 8 upcoming tracks (CJK titles).
- Tests: Rust snapshot in None/All/Shuffle/Single with and without play-next items, the 100 cap, each edit incl. `track_id` mismatch and out-of-range, jump semantics, change-only emission; Swift decoding of `QueueState` and the event, command encoding, store update.

## Track S — settings window (Swift)

Owns: Swift `YplayerKit/Protocol/*` (settings commands, event, model), new `Yplayer/Settings/SettingsWindowController.swift` + `SettingsView.swift`, `Yplayer/App/AppModel.swift`, `Yplayer/App/AppDelegate.swift`, `Yplayer/Views/LibraryTabs.swift` (gear button), `Yplayer/Views/PopoverView.swift` (⌘,), `Yplayer/Debug/SnapshotRenderer.swift` + `DebugFixtures.swift`, tests.

- S1: `ServiceSettings` model, `.settings` event, commands `settingsGet`, `settingsSet(levelLoudness:apiKey:)` (encodes only the fields given; `apiKey: .some(nil)` sends null), `libraryMove(to:)`.
- S2: opening: a gear button (`gearshape`) at the trailing end of the library tab bar, and ⌘, while the popover is open (closes the popover first). The controller creates the window on open and releases it on close; while it is open the app uses activation policy `.regular` (Dock icon, ⌘-Tab) and returns to `.accessory` on close; ⌘W and Esc close it.
- S3: `SettingsView`, a grouped `Form` about 460 pt wide:
  - Playback: toggle "Even out loudness"; footnote "Plays every song at a similar volume. Each song is measured once, in the background, after it downloads." When `loudness_available` is false the toggle is disabled with "Needs ffmpeg: brew install ffmpeg".
  - Music folder: the path with `~` for the home folder; buttons "Show in Finder" and "Move…". Move… opens an `NSOpenPanel` (directories only, can create folders, prompt "Move Here"); the target is `<chosen>/<current folder name>`; a confirmation "Move your music to <path>?" / "Playback stops for a moment while Yplayer moves the folder. The new place must be on the same disk." [Move] [Cancel]; then `library.move`, a "Moving…" state until the service is back and reports the new folder; errors appear in red under the row.
  - YouTube search: a secure field "API key" with Save and Remove; footnote "Only the yplay search command uses this."
  - Footer: "Yplayer <CFBundleShortVersionString>".
- S4: `settings.get` on open and after each reconnect while open; `.settings` events update it live.
- S5: snapshot states `settings` and `settings-no-ffmpeg`.
- Tests: command encoding (partial fields, null key), `ServiceSettings`/event decoding, the move target path, the `~` abbreviation.

## Track I — app icon (supervisor)

- `scripts/make-icon.swift` draws the icon with Core Graphics (no SF Symbols: their license excludes app icons) at every `iconset` size; `iconutil` builds `apps/macos/Packaging/AppIcon.icns` (committed); `build-app.sh` copies it into `Contents/Resources`; `Info.plist` gets `CFBundleIconFile = AppIcon`.
- Design: macOS icon grid (1024 canvas, 824 continuous-corner squircle, soft drop shadow); deep indigo → magenta gradient; a glass orb with a highlight and a white five-bar waveform.

## Supervisor after merge

- Full gates; `cargo test -- --ignored` for the real-ffmpeg and real-mpv tests; review snapshots (`upnext`, `settings`, icon).
- Reinstall (user approval first); confirm the library gets measured (DB), gains reach mpv (`volume-gain` on the service's mpv socket), Up Next edits, settings round-trip, a same-disk move and back in a temp-free way the user approves; perf budget `idle` after the backfill and `app-closed`.
- Record results in the spec.
