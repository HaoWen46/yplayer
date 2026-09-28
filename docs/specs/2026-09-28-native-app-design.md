# yplayer native app + background service — design

Status: approved in brainstorming 2026-09-28. Branch: `native-app` (off `phase0-unbreak-and-panics`, which is not yet merged to `main`).

## Goal

While watching YouTube in Safari (or Chrome), the user drags the page URL onto a floating orb; the song is added to a chosen album and starts playing within ~3 s; the single network fetch also produces the local cached file; every later play is local. The terminal UI is retired in favor of a native macOS menu-bar app backed by an always-running, near-zero-idle-cost Rust service.

## Decisions (user-approved)

- Drop behavior: add to album AND start playing immediately, even when no UI is open; the dropped song interrupts current playback; afterwards playback continues with the rest of that album.
- UI v1: SwiftUI menu-bar popover player + floating orb. No full library window in v1.
- Album choice: dragging over the orb blooms it into album bubbles; release on a bubble = that album; release on the center = last-used album; "+ New…" bubble creates an album.
- TUI: retired (Ratatui code deleted). `yplay` binary remains as the service (`yplay serve`) plus a thin CLI client.
- Removal: easy, and always confirmed with a warning, except the post-drop Undo toast (reverses the drop just made, no warning).
- Look and feel: follow Music.app / Control Center conventions so intuition transfers; modern macOS (26+) materials.
- Performance is a first-class requirement with measured acceptance budgets (see Performance budget).
- Downloads keep the native YouTube audio stream (opus/webm or m4a); no mp3 transcode; ffmpeg is no longer required.
- YouTube search stays CLI-only (`yplay search`); the app searches the local library only.

## Architecture

```
 Safari/Chrome ──drag URL──▶ Yplayer.app (SwiftUI, LSUIElement, starts at login)
                             ├─ menu-bar popover player
                             ├─ orb (hidden NSPanel, shown only during URL drags)
                             └─ Now Playing + media keys (MediaPlayer framework)
                                     │ Unix socket, newline-delimited JSON
 yplay CLI (add/play/pause/…) ───────┤ ~/Library/Application Support/yplayer/yplay.sock (dir 0700, socket 0600)
                                     ▼
                             yplay serve (Rust, LaunchAgent, KeepAlive)
                             ├─ library: SQLite, the ONLY writer
                             ├─ player: one persistent mpv (--idle=yes), event-driven IPC
                             ├─ downloader: on-demand Python yt-dlp worker, concurrent jobs
                             └─ lyrics: LRCLIB over Rust HTTP, cached in SQLite
```

- Single source of truth: the service owns SQLite, mpv, the worker, and files in the cache dir. Clients never touch the DB, mpv, or cache files.
- Crate rename: `crates/yplayer-tui` → `crates/yplayer` (package `yplayer`, bin `yplay`).
- Swift app lives in `apps/macos/` as a SwiftPM package: library target `YplayerKit` (protocol models, socket client, state store, URL parsing, position clock — all unit-testable) + executable target `Yplayer` (AppKit/SwiftUI). Built with Command Line Tools only (no Xcode). `platforms: [.macOS(.v26)]` (swiftc's default target is newer than the running OS; LaunchServices rejects such bundles — verified in spike).
- App bundle assembled by script (Info.plist with LSUIElement=true, ad-hoc `codesign --sign -`), installed to `~/Applications/Yplayer.app` (not /tmp: LaunchServices refuses launch from temp dirs — verified in spike).
- Startup: two LaunchAgents in `~/Library/LaunchAgents/` — `com.yplayer.service` (runs `yplay serve`, KeepAlive=true, RunAtLoad=true) and `com.yplayer.app` (runs the app executable, LimitLoadToSessionType=Aqua). Written by `scripts/install.sh`; `just` is not installed on this machine, so scripts are the source of truth and justfile recipes only call them.
- Paths: config `dirs::config_dir()/yplayer/config.toml` (= `~/Library/Application Support/yplayer/config.toml` on macOS); socket and logs in `~/Library/Application Support/yplayer/`; cache dir unchanged (`~/Music/yt-audio`); session state stays at `<cache>/.yplayer_state.json`.
- Worker Python: launchd gives no cwd and no shell env, so `scripts/install.sh` writes the absolute `worker_python` (repo `.venv/bin/python3`) into config.toml; `find_python`'s walk-up remains only as a fallback.
- The service needs no YouTube API key (metadata comes from yt-dlp). The key remains only for `yplay search`, read from config.toml or `YT_API_KEY`.

## Socket protocol (v1)

- Transport: Unix stream socket; UTF-8 JSON, one object per line; max line 1 MiB.
- Request: `{"id": <u64>, "cmd": "<name>", ...args}`. Response: `{"id": N, "ok": true, "result": {...}}` or `{"id": N, "ok": false, "error": {"code": "<snake_case>", "message": "<human>"}}`.
- Events: `{"event": "<name>", ...}`, sent only on connections that issued `subscribe`. Events never carry `id`.
- `hello {protocol: 1}` → `{protocol: 1, server_version}`; mismatched major version → error `protocol_mismatch`.
- `subscribe` → `{snapshot: {player, library_version}}`; the client then calls `library.get` once and applies incremental events.
- `library.get` → `{tracks: [Track], albums: [Album]}` where Track = `{id, title, uploader, duration, state, added_at, last_played, thumb_path, audio_path}` and Album = `{id, name, track_ids: [ordered], created_at, last_used_at}`.
- `add {url, album: {id} | {name} | null, play: bool}` → `{track_id, album_id, was_new, was_in_album}`; `album: null` = last-used album, creating "Inbox" if no album exists; progress arrives via `download` events.
- Playback: `play {track_id, context: {album_id} | {library: true}}`, `pause`, `resume`, `toggle`, `stop`, `next`, `prev`, `seek {position}`, `volume {value 0..100}`, `loop {mode: none|single|all|shuffle}`, `queue.play_next {track_id}`.
- Library edits: `album.create {name}`, `album.rename {album_id, name}`, `album.delete {album_id}` (tracks stay in library), `album.add {album_id, track_id}`, `album.remove {album_id, track_id}`, `album.reorder {album_id, track_ids}`, `track.delete {track_id, to_trash: bool (default true)}`, `track.rename {track_id, title}`, `track.retry {track_id}`, `rescan`.
- `lyrics {track_id}` → `{synced: bool, lines: [{t_ms, text}] } | {missing: true}`.
- `now` → `{player, track}` (CLI convenience).
- Events: `player {state: playing|paused|stopped, track_id, context, position, at_ms, duration, volume, loop}` (emitted only on change: play/pause/seek/track change/volume/loop; `at_ms` = wall-clock ms when `position` was sampled); `track.upsert {track}`; `track.removed {track_id}`; `album.upsert {album}`; `album.removed {album_id}`; `download {track_id, phase: fetching|downloading|done|failed|cancelled, bytes, total, error?}` (≤2/s per track); `toast {severity: info|warn|error, message}`; `resync` (the connection lagged behind the event stream; the client must call `library.get` again).
- `library_version` (u64) increments on every library mutation and is returned by `subscribe` and `library.get`.
- Library context order (Songs view and `context: {library: true}`): `added_at DESC, title COLLATE NOCASE`.

## Drop flow

1. Orb receives the drop; `YplayerKit` extracts the video id (accepts `youtube.com/watch?v=`, `youtu.be/`, `music.youtube.com/watch?v=`, `youtube.com/shorts/`; `&list=` ignored). Non-YouTube or playlist-only URL → orb shake, no request.
2. App sends `add {url, album, play: true}`.
3. Service: if the track row exists with `state=complete` and its audio file exists → link to album (if not already), bump `albums.last_used_at`, play the local file. No network.
4. Else: insert/refresh the row with `state=downloading` (title = video id placeholder, `audio_path=''`), link to album, emit `track.upsert` + `album.upsert`, then send a download job to the worker.
5. Worker: one `extract_info(url, download=True)` call (a single metadata fetch) with `format=bestaudio`, `nopart=True`, `outtmpl='<cache>/%(title).150B [%(id).8s]/audio.%(ext)s'`, `extractor_args={'youtube': {'skip': ['hls', 'dash']}}`, and NO `writethumbnail` (yt-dlp probes thumbnails best-first before the media download — measured ~9 s of misses on the test video) → the first progress callback with bytes on disk emits `started {path, dir, meta}` (service sets title/uploader/duration/audio_path) → throttled `progress` → after the audio completes, one GET of `https://i.ytimg.com/vi/<id>/hqdefault.jpg` → `<dir>/cover.jpg` (non-fatal) → writes `meta.json` LAST → final `ok` response.
6. On `started`: service tells mpv `loadfile appending://<abs path>` (plays the growing file; measured ~3.2 s to first audio vs 11.6–12.5 s today).
7. On final `ok`: row → `state=complete`, `audio_path`, `thumb_path`, `file_size`; emit `track.upsert` + `download done`.
8. Play context after a drop-play: queue = [dropped track] + the album's other tracks in album order.
9. Skip/quit/seek during a download never affects the download; the cache still gets the full file (a seek beyond downloaded bytes blocks until data arrives — measured).
10. Failure: row → `state=failed`, delete the track's folder (service-created, known path), emit `download failed` + error toast; `track.retry` re-runs step 4.
11. Undo (toast, 6 s): if `!was_in_album` → `album.remove`; if `was_new` → `track.delete {to_trash: false}` (cancels any in-flight download, stops playback if current).

## Playback engine

- One mpv process: `--idle=yes --no-video --no-terminal --input-ipc-server=<sock> --input-media-keys=no --demuxer-max-bytes=32MiB --demuxer-max-back-bytes=8MiB`; audio output chosen explicitly after investigating the audit's "coreaudio -50 then avfoundation fallback (+88 ms)" log.
- Event-driven: `observe_property` for `pause`, `playlist-pos`, `duration`, `idle-active`, `volume`; handle `end-file`, `playback-restart`, `seek`. `time-pos` is NEVER observed or polled; the service samples it once on `playback-restart`/`seek`/pause-toggle to emit a `player` event.
- Gapless: the service keeps mpv's playlist at [current, next] by `loadfile <next> append` when the current track starts; on `playlist-pos` advance it appends the following one. Removes the measured ~0.5–0.6 s inter-track gap from per-track spawns.
- Cached files load by plain path; in-progress downloads load via `appending://`.
- Idle shutdown: after 10 minutes stopped or paused, the service records `{track_id, position}` and quits mpv (frees ~45 MB); resume respawns mpv and loads with `start=<position>` (~0.35 s measured spawn-to-audio).
- mpv crash/exit: supervisor marks the player stopped, emits a warn toast, respawns lazily on the next playback command.
- Runtime: tokio `current_thread`; blocking work (reconcile, lyrics HTTP, update check) on `spawn_blocking`; one core actor owns the DB connection, engine and download table (no locks).
- No periodic timers in the service. The only timers are one-shot: mpv idle shutdown, worker idle shutdown, daily yt-dlp update check.

## Downloader (Python worker)

- Spawned on demand; the service closes its stdin after 60 s with no in-flight jobs (worker exits on EOF). Respawn cost ~0.2 s (measured).
- Concurrent: worker runs jobs on a thread pool (max 3); stdout writes guarded by a lock; every message carries the request id. The Rust `WorkerHandle` supports streamed multi-message responses (interim `event` messages, then one terminal `ok`/error).
- Cancel: `{"cmd": "cancel", "target": <id>}` sets a flag checked in the yt-dlp progress hook, which raises `yt_dlp.utils.DownloadCancelled`; terminal response has `cancelled: true`; the service deletes the folder.
- Folder naming: `<Title> [<id8>]` via the yt-dlp output template (yt-dlp's filename sanitization, unicode kept, no `restrictfilenames`); never derived from the Data API (fixes duplicate folders when a key is/isn't present).
- Removed from the request path: `_auto_update_ytdlp` pip upgrade on any `DownloadError` (audit F1), `ensure_ytdlp_uptodate` at worker start, Data API metadata lookup in download, FFmpegExtractAudio / FFmpegMetadata / EmbedThumbnail, second `meta.json` read-back, legacy flat `<id>.json` sidecar writes, lyrics code.
- yt-dlp updates: owned by the service; at most once per 24 h, and only when no job is in flight; check latest version via the GitHub `releases/latest` redirect (HEAD request, not the 1.2 MB PyPI JSON); upgrade with `uv pip install --python <worker_python> -U "yt-dlp[default]"`; then restart the idle worker. Throttle stamp lives in the state dir, not a hard-coded path.
- Dependency: `yt-dlp[default]` (brings yt-dlp-ejs for YouTube JS challenges; `deno` is installed). `python-dotenv` stays only for CLI search convenience.
- Search/list_formats/playlist_entries worker commands remain for the CLI and are otherwise unchanged in this project.

## Library and storage

- Startup trusts SQLite; no blocking scan (audit: full meta.json scan cost 0.37 s at 5k tracks, 2.3 s at 20k).
- Background reconcile at startup and on `rescan`: one `read_dir` of the cache root; import only directories not referenced by any row's `audio_path` (parse their `meta.json`; skip dirs without `meta.json` and log them — never auto-delete unknown user files); delete rows whose audio file is gone; legacy flat `<id>.<ext>` files still supported.
- Startup recovery: rows left in `state=downloading` (service died mid-download) → delete their folder, set `state=failed`.
- Legacy `<cache>/albums/*.album.json` files are imported exactly once, inside the 0 → 1 migration (the old scanner re-imported them on every launch, which would undo album edits).
- Schema migration via `PRAGMA user_version` 0 → 1: `tracks.state TEXT NOT NULL DEFAULT 'complete'`; `tracks.thumb_path TEXT`; `albums.last_used_at INTEGER`; `CREATE INDEX idx_album_tracks_track ON album_tracks(track_id)`; `CREATE TABLE lyrics (track_id TEXT PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE, synced INTEGER, body TEXT, fetched_at INTEGER NOT NULL)` (`body NULL` = confirmed miss).
- Use `prepare_cached` for hot statements. Existing WAL/NORMAL pragmas stay.
- `track.delete`: cancel in-flight download, if current track then advance to next (or stop), remove row (cascades album links + lyrics), move the track folder to the macOS Trash via the `trash` crate (NSFileManager path, no Finder/AppleScript) or delete permanently when `to_trash: false`.
- Existing mp3 library remains playable; new downloads are opus/webm or m4a.

## Lyrics

- Fetched by the service in Rust from LRCLIB (port `_clean_track_title` from `yplayer/core.py` with its existing behavior and test cases — it strips `Artist『Song』MV(...)` decorations and is required for matches).
- Order: exact `/api/get` first, then `/api/search` with de-duplicated candidates; 10 s per call; a 429 stops the lookup; a transport error or 5xx only skips that call (LRCLIB latency is erratic — `/api/get` measured 1 s to >6 s, `/api/get-cached` 17 s); a lookup with any failed call and no hit is an error, never a cached miss.
- Cached in the `lyrics` table, including misses; misses retried after 7 days; transient errors are not cached.
- Fetched only when the app requests lyrics for a track (lyrics view open); stale requests for tracks no longer displayed are dropped.
- HTTP client: `/usr/bin/curl` subprocess behind an `HttpGet` trait (no TLS stack compiled into the binary; calls are rare), run on a blocking thread; the same client serves the yt-dlp update check.

## Menu-bar app (sub-project 2)

- Status item: SF Symbol music note; glyph changes only on play/pause state change (no animation).
- Popover (~340×520, system glass material): now-playing card (artwork, title, artist, scrubber, ⏮ ⏯ ⏭, volume, lyrics toggle, ⋯ menu) above a segmented control: Albums · Songs · Search.
- Interactions (Music.app conventions): click row = play in that context; right-click menu = Play Next, Add to Album ▸, Remove from Album… (album view only), Delete from Library…, Show in Finder, Copy YouTube Link, Retry (failed only); trackpad swipe-left on a row = Remove (in an album) or Delete (in Songs); drag rows to reorder within an album; drag a row onto an album = add; ⌘F focuses search; Space play/pause; ⌫ = remove from album in an album view, delete from library in Songs (always confirmed); ⌘⌫ delete from library (confirmed); Esc closes.
- Albums: + creates; double-click title renames; delete album asks for confirmation (songs stay in library).
- Confirmations: "Remove 'X' from <Album>?" (song stays in library); "Delete 'X' from your library? The <size> file moves to the Trash." (destructive red button).
- Downloading rows show a progress ring; failed rows show a retry affordance.
- Lyrics: synced lines highlighted by the local position clock.
- Artwork: `cover.<ext>` sidecar (`cover.jpg` for new downloads; legacy covers may be webp, which decodes natively), decoded at display size, in-memory cache.
- Now Playing / media keys: `MPNowPlayingInfoCenter` (title, artist, duration, elapsed, rate, artwork, `playbackState`) updated only on `player` events; `MPRemoteCommandCenter` play/pause/toggle/next/prev/seek forward to the service.
- Connection: on socket loss show "Reconnecting…" and retry with backoff 1 s → 30 s max; re-`subscribe` + `library.get` on reconnect.

## Orb (sub-project 3)

- One borderless `NSPanel` created at launch and kept ordered out: `styleMask` nonactivatingPanel, `level = .statusBar`, `collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary, .ignoresCycle]`, `hidesOnDeactivate = false`, app activation policy `.accessory`; shown with `orderFrontRegardless()`; no fully transparent pixels under the drop zone; `draggingEntered` returns `.copy`.
- Drag detection without any permission: global `NSEvent` monitor for `.leftMouseDragged` compares `NSPasteboard(name: .drag).changeCount` with the last seen value and inspects types only on change; show the orb when types include `public.url` (or a plain-text string that parses as a YouTube URL). Hide 0.3 s after `.leftMouseUp`. Fallback if global monitors go quiet during a drag: while the orb is visible, poll `NSEvent.pressedMouseButtons` at 10 Hz; stop polling when hidden.
- Placement: right edge, vertically centered, of the screen containing the cursor.
- Bloom: on `draggingEntered` the orb expands into up to 5 album bubbles (by `last_used_at` desc) plus "+ New…"; center = last-used album (or "Inbox" when no albums exist). Spring animations only while visible.
- Drop on "+ New…": activate the app and show a small name field (default "New Album"), then `album.create` + `add`.
- Drop registration types: `.URL`, `.string`; read with `readObjects(forClasses: [NSURL.self])`, falling back to string parsing.
- After a successful drop: Undo toast (6 s) anchored near the orb/status item.
- Supported drag sources (research): Safari address field and page links, Chrome location icon and page links. Not supported: Chrome tabs (never produce an external drag). Safari tab drags: unknown until manually tested.

## Performance budget (acceptance criteria)

Measured with `top -l <n> -s 1 -stats pid,command,cpu,idlew,mem,threads` over 20 s windows, same method as the 2026-09-28 audit.

| State | App | Service | mpv | Python worker |
|---|---|---|---|---|
| Nothing playing, popover closed | 0 idle wakeups/s, <45 MB (measured: 14 MB before first open, ~41 MB after use) | 0 idle wakeups/s, <10 MB | not running (after 10 min) | not running |
| Playing, popover closed | ~0 (events only on track change) | no timers | ~2.5% CPU, ≤60 MB | not running |
| Popover open, playing | scrubber redraw ≤1/s from local clock | — | — | — |
| Downloading | — | — | — | running; exits 60 s after last job |

- Library to app: one `library.get` (~500 KB at 5k tracks), then incremental events; search filters locally per keystroke (<1 ms at 5k, case/diacritic/width-insensitive); SwiftUI lists are lazy.
- Position: no periodic IPC; the app extrapolates `position + (now - at_ms)` while playing.
- Service baseline to beat (audit): TUI idle 0% CPU, 2.35 wakeups/s, 2.6 MB; paused 4.15 wakeups/s with 4.5 empty redraws/s and 8 IPC requests/s; mpv 58 MB playing; two idle Python workers 75.6 MB.
- First audio for a new URL: target ≤4 s on the test URL (measured 3.15–3.5 s in spike vs 11.6–12.5 s today).
- A perf script (`scripts/perf-budget.sh`) automates the service rows; app rows are measured the same way by hand.

## Error handling

- Socket lost (app): "Reconnecting…" state, backoff retry; launchd restarts the service.
- Download failure: `state=failed`, folder deleted, error toast with yt-dlp's message, Retry in the context menu.
- mpv missing/crash: error toast; cached playback resumes after respawn; missing binary → persistent warn in the popover.
- Worker unavailable (Python/venv broken): error toast "Downloader unavailable: <reason>"; cached playback unaffected.
- Non-YouTube drop: orb shake. Playlist-only URL: warn toast "Playlists are not supported yet". `watch?v=…&list=…`: single video.
- Unknown/invalid socket request: error response with `code`; the connection stays open.

## Testing

- Rust unit: protocol serde round-trips; YouTube URL → id parsing; queue/context logic (pure); schema migration 0→1 on a fixture DB; album ops; reconcile on a temp dir; LRC parsing (existing) and `clean_track_title` ported cases.
- Rust integration: `yplay serve` on a temp socket + temp cache with the fake worker (`YPLAY_WORKER_CMD`) and real mpv `--ao=null` on a committed tiny audio fixture; drive via the CLI client; assert events and DB state.
- Python: worker protocol tests with yt-dlp mocked (streamed events, concurrency, cancel); one opt-in live network test (existing `-- --ignored` pattern).
- Swift: `YplayerKit` unit tests (URL parsing, position clock, event reducer, protocol decoding). First task of sub-project 2 verifies `swift test` works with Command Line Tools only; fallback is a test executable target with assertions.
- Performance: `scripts/perf-budget.sh` for service rows; manual measurement for app rows.
- Manual: real drags from Safari address field, Safari link, Chrome location icon, over full-screen Safari; Now Playing and media keys; delete/undo flows.

## Sub-projects and done criteria

1. Service core (Rust + Python): crate rename; delete Ratatui UI and its deps (ratatui, crossterm, fuzzy-matcher, unicode-width); `yplay serve`; socket protocol v1; persistent event-driven mpv + gapless queue + idle shutdown; worker concurrency/streaming/cancel/native format/no pip on request path; service-owned yt-dlp update; schema migration; reconcile; delete-to-Trash; Rust lyrics; CLI client (`yplay add <url> [--album NAME] [--no-play]`, `play|pause|toggle|next|prev|stop`, `now`, `albums`, `search <query>`); LaunchAgent install script. Done = `yplay add <test URL>` plays in ≤4 s, a second add of the same URL plays with zero network, service rows of the performance budget pass, all gates green (`cargo fmt --check`, clippy `-D warnings`, `cargo test`, ruff, pytest).
2. Menu-bar app (Swift): popover player, Albums/Songs/Search, removal with confirmations, Now Playing + media keys, lyrics, reconnect, login start. Done = all interactions above work against the real service; app rows of the budget pass.
3. Orb (Swift): drag detection, bloom, drop → add, "+ New…", Undo toast. Done = manual drag matrix passes (Safari address field, Safari link, Chrome location icon, full-screen Safari).

Each sub-project gets its own implementation plan and is completed and verified before the next starts.

## Verification items (resolve during implementation, not assumptions)

- `MPNowPlayingInfoCenter` accepts the app as Now Playing while audio comes from mpv in another process; fallback: enable mpv's own media-key/Now Playing integration and mirror metadata.
- RESOLVED (SP1 T16): coreaudio rejects mpv's default planar-float format on macOS 27, so mpv fell back to avfoundation (2 s device + 2 s soft buffer): playlist handoffs fired ~3.5 s before the audio and a final track could lose its tail. Fix: mpv runs with `--audio-format=float` → coreaudio, 13 ms device latency, 0.2 s buffer; a 19.02 s clip now hands off at 18.89 s and plays in full standalone.
- `appending://` end-of-file wait (~0.4–0.7 s measured) does not break gapless preload for a just-downloaded current track.
- Safari tab drag types; global drag monitor behavior during Safari drags.
- `swift test` availability with Command Line Tools.

## SP1 verification results (2026-09-28, installed LaunchAgent, real library)

- Idle (nothing loaded): service 0.00 % CPU, 0.00 idle wakeups/s, 3.1 MB; no mpv, no worker (TUI baseline: 2.35 wakeups/s).
- Playing: service 0.00 wakeups/s, 3.5 MB; mpv 2.9 % CPU, 46 MB.
- New URL (`yplay add --wait`, worker cold start included): first audio 3.7 s, downloaded 4.9 s; cache hit: `cached — playing` in 0.009 s with no worker request.
- Handoff: one mpv process across tracks; natural handoff 0.13 s before end of file (coreaudio buffer).
- Idle shutdown: paused at 0:04 → mpv gone after 10 min → idle budget PASS (service 6.2 MB after activity) → `resume` respawned mpv and continued from 0:04.
- Unavailable video: fails in 2.1 s with yt-dlp's message; no pip run. Undo mid-download: cancelled in 10 ms, partial folder removed. Delete: folder moved to Trash.
- Lyrics: synced lines fetched in 3.8 s, cached reads instant; LRCLIB latency is erratic (see Lyrics).
- Migration: 8 existing tracks preserved, DB backed up to `.yplayer.db.pre-service.bak`; a folder with `meta.json` but no audio was left untouched.
- Known gaps: VBR mp3s from the old pipeline seek inaccurately (mpv estimates positions; a seek near the end can land at the end); a single player change emits 3–4 near-identical `player` events (clients should coalesce).

## Out of scope (v1)

- Full library window; YouTube search/browse in the app; playlist import; browser extensions; bookmarklets; `yplay://` URL scheme and Shortcuts/share-sheet integration; Safari tab drags; lyrics editing; cross-platform; notarization/distribution beyond this machine; homebrew `yt-dlp` (it is outdated and broken — the app always uses its own venv copy).

## Evidence

- Research and audits ran 2026-09-28; raw outputs and spike scripts are in the session scratchpad (`spike-app/`, `stream-spike/`, `rust-audit/`, `py-audit/`), not committed.
- Key measurements: first audio 11.6–12.5 s (mp3 path) vs 3.15–3.5 s (native + appending://); mp3 2.24× larger than native opus; yt-dlp import 116–126 ms; worker ready 154–200 ms; idle worker 37.8 MB each; mpv spawn→IPC 172–184 ms; inter-track gap ~0.5–0.6 s; scan 358–376 ms at 5k tracks; library render 34.6% CPU at 5k tracks (O(N²) `playing_index` per row — dies with the TUI).
