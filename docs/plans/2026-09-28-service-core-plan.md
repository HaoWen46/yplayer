# Sub-project 1 — service core: implementation plan

Spec: `docs/specs/2026-09-28-native-app-design.md` (read it first; this plan does not repeat its rationale). Branch: `native-app`.

Goal: replace the Ratatui TUI with `yplay serve` (a background service owning SQLite, one persistent mpv, an on-demand yt-dlp worker, and LRCLIB lyrics) plus a thin `yplay` CLI client, meeting the spec's service performance budget.

## Execution model

- Tasks are executed by subagents; the supervisor reviews every diff against this plan and the spec, runs all gates, and merges.
- Waves: tasks inside one wave touch disjoint files and run in parallel in separate git worktrees; waves run in order. Wave 0: T1. Wave 1: T2+T3 (agent A), T4+T5 (agent B), T6 (agent C), T7+T8 (agent D). Wave 2: T9 (agent E), T10+T11 (agent F). Wave 3: T12, then T13, then T14 (sequential). Wave 4: T15, then T16 (supervisor).
- Each task ends with one commit (or one commit per task when an agent owns two tasks) whose message starts with `SP1 T<n>:` and ends with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Merge conflicts expected only in `crates/yplayer/src/lib.rs` (module lines) and `crates/yplayer/Cargo.toml` (deps); the supervisor resolves them.

## Rules for every task

- Test first: write the failing tests named in the task, run them to see them fail, implement, run them to pass.
- Gates that must pass before committing: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `.venv/bin/ruff check yplayer/ tests/`, `.venv/bin/python -m pytest -q`.
- Python is run only via the main checkout's venv: `/Users/haowenchen/Files/projects/yplayer/.venv/bin/python` (and `.venv/bin/ruff`), also from worktrees (worktrees have no `.venv`); run pytest from the worktree root so the worktree's `yplayer` package shadows the editable install; installing packages uses `uv pip install --python <that python> ...` (never pip directly); never run `uv pip install -e .` from a worktree (it would repoint the editable install).
- Touch only the files the task names; do not refactor unrelated code; match surrounding style and comment density.
- Tests that need real `mpv` or the network are `#[ignore]` (Rust) or marked `@pytest.mark.live` and skipped by default (Python); state in the commit message how you ran them.
- No new dependencies beyond those a task names.
- Public items live in the library crate, so unused-for-now pub APIs do not trigger `dead_code`; do not add `#[allow(dead_code)]`.
- Return to the supervisor: files changed, test names added, gate results (pass/fail per gate), commit hash, and any deviation from this plan with the reason.

## Shared contracts (all tasks code against these exact shapes)

### Crate layout after T1

- `crates/yplayer/` package `yplayer`, lib `yplayer` (`src/lib.rs`) + bin `yplay` (`src/main.rs`).
- Modules (created by the task in brackets): `config` [T1], `types` [T1], `protocol` [T2], `ytid` [T2], `player::queue` [T3], `library::db` [T4], `library::reconcile` [T5], `http` [T7], `lyrics` [T1 moves `parse_lrc`; T7 completes], `updater` [T8], `download::bridge` [T1 trims; T9 extends], `download::worker` [T9], `player::mpv` [T10], `player::engine` [T11], `service` [T12, T13], `client` [T14].
- Runtime: `#[tokio::main(flavor = "current_thread")]`; tokio features `rt, macros, process, net, io-util, sync, time, signal`.

### types.rs (T1)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackState { Complete, Downloading, Failed }            // DB TEXT: "complete" | "downloading" | "failed"

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,                    // YouTube video id (11 chars) or legacy id
    pub title: String,
    pub uploader: Option<String>,
    pub duration: Option<i64>,         // seconds
    pub webpage_url: Option<String>,
    pub audio_path: Option<String>,    // DB stores '' for unknown; exposed as None
    pub format: Option<String>,        // file extension, e.g. "webm", "m4a", "mp3"
    pub file_size: Option<i64>,
    pub added_at: Option<i64>,         // unix seconds
    pub last_played: Option<i64>,
    pub state: TrackState,
    pub thumb_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Album { pub id: i64, pub name: String, pub track_ids: Vec<String>, pub created_at: i64, pub last_used_at: Option<i64> }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoopMode { None, Single, All, Shuffle }
```

### Socket protocol v1 (T2)

- One JSON object per line, UTF-8, max 1 MiB per line (`pub const MAX_LINE: usize = 1 << 20`). `pub const PROTOCOL: u32 = 1`.
- `pub struct RequestEnvelope { pub id: u64, #[serde(flatten)] pub cmd: Command }`.
- `Command` is `#[serde(tag = "cmd")]` with these exact wire names and fields: `hello {protocol: u32}`, `subscribe`, `library.get`, `now`, `add {url: String, album: Option<AlbumRef>, play: bool (default true)}`, `play {track_id, context: ContextRef}`, `pause`, `resume`, `toggle`, `stop`, `next`, `prev`, `seek {position: f64}`, `volume {value: f64}`, `loop {mode: LoopMode}`, `queue.play_next {track_id}`, `album.create {name}`, `album.rename {album_id: i64, name}`, `album.delete {album_id}`, `album.add {album_id, track_id}`, `album.remove {album_id, track_id}`, `album.reorder {album_id, track_ids: Vec<String>}`, `track.delete {track_id, to_trash: bool (default true)}`, `track.rename {track_id, title}`, `track.retry {track_id}`, `rescan`, `lyrics {track_id}`.
- `AlbumRef`: untagged `{"id": i64}` | `{"name": String}`. `ContextRef`: untagged `{"album_id": i64}` | `{"library": true}`; Rust enum `ContextRef::Album(i64) | ContextRef::Library` with custom (de)serialize producing exactly those shapes.
- `Response { id: u64, ok: bool, result: Option<Value> (omit when None), error: Option<ErrorBody> (omit when None) }`; `ErrorBody { code: ErrorCode, message: String }`; `ErrorCode` snake_case: `bad_request, unknown_command, protocol_mismatch, not_found, conflict, invalid_url, unsupported_url, downloader_unavailable, player_unavailable, internal`. Constructors `Response::ok(id, Value)` and `Response::err(id, code, msg)`.
- Results: `hello` → `{protocol, server_version}`; `subscribe` → `{player: PlayerState, library_version: u64}`; `library.get` → `{tracks: [Track], albums: [Album], library_version}`; `now` → `{player: PlayerState, track: Track|null}`; `add` → `AddResult {track_id, album_id, was_new, was_in_album}`; `album.create` → `{album: Album}`; `lyrics` → `{synced: bool, lines: [{t_ms: i64|null, text}]}` (`t_ms` null when unsynced) or `{missing: true}`; everything else → `{}`.
- `PlayerState { state: PlayState (playing|paused|stopped), track_id: Option<String>, context: Option<ContextRef>, position: f64, at_ms: i64, duration: Option<f64>, volume: f64, #[serde(rename = "loop")] loop_mode: LoopMode }`.
- `Event` is `#[serde(tag = "event")]`: `player` (PlayerState fields flattened), `track.upsert {track}`, `track.removed {track_id}`, `album.upsert {album}`, `album.removed {album_id}`, `download {track_id, phase: fetching|downloading|done|failed|cancelled, bytes: Option<u64>, total: Option<u64>, error: Option<String>}`, `toast {severity: info|warn|error, message}`, `resync`.
- Codec: `pub fn encode_line<T: Serialize>(v: &T) -> Vec<u8>` (JSON + `\n`); `pub fn decode_request(line: &str) -> Result<RequestEnvelope, Response>` returning a ready `bad_request`/`unknown_command` error Response (id from the JSON if present, else 0) on failure.

### ytid.rs (T2)

- `pub enum UrlError { NotYouTube, PlaylistOnly, NoVideoId }`; `pub fn parse_video_id(input: &str) -> Result<String, UrlError>`.
- Accept (scheme optional, any of `www.`/`m.`/`music.` hosts): `youtube.com/watch?v=ID` (any param order, `&list=` ignored), `youtu.be/ID[?…]`, `youtube.com/shorts/ID`, `youtube.com/live/ID`, `youtube.com/embed/ID`, and a bare 11-char id. ID = exactly 11 of `[A-Za-z0-9_-]`. `youtube.com/playlist?list=…` or `watch?list=…` without `v` → `PlaylistOnly`. Other hosts → `NotYouTube`. No `url`/`regex` crate: hand-parse.

### library::db (T4) — `pub struct Db`

- `open(db_path: &Path, cache_dir: &Path) -> Result<Db>`: pragmas (`journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5000`), create v0 tables if absent, migrate to `user_version = 1`, cleanup orphan `album_tracks`.
- Migration 0→1 in one transaction: `ALTER TABLE tracks ADD COLUMN state TEXT NOT NULL DEFAULT 'complete'`; `ALTER TABLE tracks ADD COLUMN thumb_path TEXT`; `ALTER TABLE albums ADD COLUMN last_used_at INTEGER`; `CREATE INDEX IF NOT EXISTS idx_album_tracks_track ON album_tracks(track_id)`; `CREATE TABLE IF NOT EXISTS lyrics (track_id TEXT PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE, synced INTEGER, body TEXT, fetched_at INTEGER NOT NULL)`; one-time import of `<cache_dir>/albums/*.album.json` (format: `{name, description, tracks: [{id, order}]}`; get-or-create album by name; `INSERT OR IGNORE` links to existing tracks only); `PRAGMA user_version = 1`.
- Tracks: `get_track(id) -> Option<Track>`; `list_tracks() -> Vec<Track>` ordered `added_at DESC, title COLLATE NOCASE`; `library_order() -> Vec<String>` (same order); `insert_pending(id, webpage_url)` (new row: title = id, audio_path = '', state downloading, added_at now; existing row: set state downloading only); `mark_started(id, &TrackMeta, audio_path)` (sets title/uploader/duration/webpage_url/audio_path); `mark_complete(id, &TrackMeta, audio_path, format, file_size, thumb_path)` (state complete); `mark_failed(id)`; `upsert_track(&Track)`; `rename_track(id, title)`; `delete_track(id)`; `touch_last_played(id)`; `tracks_in_state(TrackState) -> Vec<Track>`.
- `pub struct TrackMeta { pub id: String, pub title: String, pub uploader: Option<String>, pub duration: Option<i64>, pub webpage_url: Option<String> }` lives in `types.rs` (added by T4).
- Albums: `list_albums() -> Vec<Album>` (ordered `name COLLATE NOCASE`, `track_ids` in position order); `get_album(id) -> Option<Album>`; `album_id_by_name(name) -> Option<i64>`; `create_album(name) -> Result<i64>` (duplicate name → `DbError::Conflict`); `get_or_create_album(name) -> i64`; `rename_album(id, name)` (conflict on duplicate); `delete_album(id)`; `album_add(album_id, track_id) -> bool` (append at `MAX(position)+1`; false if already present); `album_remove(album_id, track_id) -> bool`; `album_reorder(album_id, &[String])` (must be a permutation of current ids, else `DbError::BadRequest`; rewrites positions 0..n in a transaction); `albums_containing(track_id) -> Vec<i64>`; `touch_album(id)` (last_used_at = now); `last_used_album() -> Option<i64>` (max last_used_at, ties → lowest id; albums never used are ignored).
- Lyrics: `get_lyrics(track_id) -> Option<LyricsRow { synced: bool, body: Option<String>, fetched_at: i64 }>`; `put_lyrics(track_id, synced, body: Option<&str>)` (upsert; `body None` = miss).
- Errors: `pub enum DbError { NotFound, Conflict, BadRequest(String), Sqlite(rusqlite::Error) }` implementing `std::error::Error`; methods return `Result<T, DbError>`.
- Use `prepare_cached` for every statement executed per request.

### library::reconcile (T5)

- `pub struct ReconcileReport { pub imported: Vec<String>, pub removed: Vec<String>, pub skipped_dirs: Vec<PathBuf>, pub deleted_partials: Vec<PathBuf> }`.
- `pub fn reconcile(cache_dir: &Path, db: &Db, in_flight: &HashSet<String>) -> Result<ReconcileReport>`: one `read_dir(cache_dir)`; known = parent dirs of per-track `audio_path`s plus flat `audio_path`s; for each unknown dir: with `meta.json` → parse, find audio (extensions `mp3 m4a opus flac wav webm ogg oga aac`) and `cover.*` (`webp jpg jpeg png`), import as `state complete` (skip if the id already exists); without `meta.json` → if its name ends with `[<id8>]` and `id8` is the 8-char prefix of a track in state `downloading`/`failed` that is not in `in_flight`, remove the dir (partial) and record it in `deleted_partials`, else record in `skipped_dirs` and leave it; legacy flat `<id>.<ext>` audio files (with optional `<id>.json` sidecar) not known → import; finally delete rows (state `complete` only) whose `audio_path` file no longer exists → `removed`.
- `pub fn recover_interrupted(cache_dir: &Path, db: &Db) -> Result<Vec<String>>`: rows in state `downloading` → delete the parent dir of a non-empty `audio_path` (only if the dir name ends with `[<id8>]`), set state `failed`; return ids.
- Never deletes a directory that has a `meta.json`.

### http.rs (T7)

- `pub struct HttpResponse { pub status: u16, pub body: Vec<u8>, pub location: Option<String> }`.
- `pub trait HttpGet: Send + Sync { fn get(&self, url: &str, timeout: Duration) -> anyhow::Result<HttpResponse>; fn head_no_redirect(&self, url: &str, timeout: Duration) -> anyhow::Result<HttpResponse>; }` (blocking; callers use `spawn_blocking`).
- `pub struct CurlHttp { pub user_agent: String }` implements it with `/usr/bin/curl` (fallback `curl` on PATH): `-sS --max-time <secs> -A <ua>`; status via `-w`; `head_no_redirect` uses `-I` without `-L` and returns the `Location` header. A non-2xx status is `Ok(HttpResponse)`; only process/transport failure is `Err`.

### lyrics.rs (T1 + T7)

- `pub fn parse_lrc(lrc: &str) -> Vec<(f64, String)>` (moved unchanged from `app.rs` with its test in T1).
- `pub fn clean_track_title(title: &str) -> String`: port of `yplayer/core.py::_clean_track_title` using `regex-lite`.
- `pub enum LyricsOutcome { Found { synced: bool, body: String }, Missing, Error(String) }`.
- `pub fn fetch(http: &dyn HttpGet, title: &str, artist: Option<&str>, duration: Option<i64>) -> LyricsOutcome`: base `https://lrclib.net`; UA `yplayer (https://github.com/HaoWen46/yplayer)`; 6 s timeout per call; step 1 (only when artist and duration are known) `GET /api/get?track_name=<cleaned>&artist_name=<artist>&duration=<d>` → 200 with `syncedLyrics` → Found synced; step 2 `GET /api/search?track_name=&artist_name=` over de-duplicated candidates `[(cleaned, artist), (cleaned, ""), (raw, artist)]` (skip empty track names) → pick the synced hit with the closest duration (as the Python did) → Found synced; if no synced hit but some `plainLyrics` → Found `synced: false`; transport error, 429 or 5xx on any call → `Error` (stop); otherwise `Missing`.

### updater.rs (T8)

- `pub fn parse_version(s: &str) -> Option<Vec<u64>>` (dotted integers, e.g. `2026.06.09` or `2026.06.09.1`); `pub fn is_newer(latest: &str, current: &str) -> bool`; `pub fn version_from_location(loc: &str) -> Option<String>` (last path segment after `/tag/`).
- `pub trait CmdRunner: Send + Sync { fn run(&self, program: &str, args: &[&str], timeout: Duration) -> anyhow::Result<CmdOutput { status: i32, stdout: String, stderr: String }>; }` + `pub struct SystemRunner`.
- `pub enum UpdateOutcome { UpToDate(String), Upgraded { from: String, to: String }, Skipped(String), Failed(String) }`.
- `pub struct Updater { pub python: String, pub uv: Option<String>, pub stamp: PathBuf, pub http: Arc<dyn HttpGet>, pub runner: Arc<dyn CmdRunner> }` with `fn due(&self, now: SystemTime) -> bool` (stamp missing or mtime older than 24 h) and `fn check_and_upgrade(&self) -> UpdateOutcome` (blocking): current via `<python> -c "import yt_dlp.version as v; print(v.__version__)"`; latest via `head_no_redirect("https://github.com/yt-dlp/yt-dlp/releases/latest")` → `Location`; if newer and `uv` known: `uv pip install --python <python> -U "yt-dlp[default]"` (timeout 180 s); touch the stamp on `UpToDate`/`Upgraded`; `Skipped("uv not found")` when uv is missing.

### Worker protocol v2 (T6 Python side, T9 Rust side)

- Worker → host first line: `{"event": "ready", "ok": true, "protocol": 2}`.
- Requests: `{"id": N, "cmd": "download", "url": U, "cache_dir": D}`; `{"id": N, "cmd": "cancel", "target": M}`; `search`, `list_formats`, `playlist_entries` unchanged except they now run on the pool. `video_info` and `lyrics` commands are removed.
- Download interim lines: exactly one `{"id": N, "event": "started", "path": P, "dir": D, "meta": {id, title, uploader, duration, webpage_url}}` on the first progress callback with bytes on disk; then `{"id": N, "event": "progress", "bytes": B, "total": T|null}` at most twice per second.
- Terminal line per request: `{"id": N, "ok": true, ...}` (download adds `path, dir, meta, thumb: P|null, format: ext, file_size: int`) or `{"id": N, "ok": false, "error": "message", "cancelled": bool}`. `cancel` replies `{"id": N, "ok": true}` immediately.
- Every stdout line after `ready` carries `id`; stdout writes are serialized by a lock; handlers run on `ThreadPoolExecutor(max_workers=3)`; `cancel` is handled inline on the reader thread; on stdin EOF the worker waits for running jobs and exits 0.

### download::worker (T9) — `WorkerHandle`

- `pub struct WorkerOptions { pub worker_python: Option<String>, pub log_path: PathBuf, pub idle_timeout: Duration /*60 s*/, pub inactivity_timeout: Duration /*180 s*/ }`.
- `pub enum WorkerMsg { Started { path: String, dir: String, meta: TrackMeta }, Progress { bytes: u64, total: Option<u64> }, Done { path: String, dir: String, meta: TrackMeta, thumb: Option<String>, format: Option<String>, file_size: Option<i64> }, Failed { error: String, cancelled: bool, transport: bool } }`; `Done` and `Failed` are terminal.
- `#[derive(Clone)] pub struct WorkerHandle`; `pub fn spawn(opts: WorkerOptions) -> WorkerHandle`; `pub fn download(&self, url: String, cache_dir: PathBuf) -> (JobId, mpsc::UnboundedReceiver<WorkerMsg>)`; `pub fn cancel(&self, job: JobId)`; `pub fn restart_when_idle(&self)`; `pub fn busy(&self) -> bool` (any job in flight).
- Actor: spawns the worker lazily on the first job; a reader task routes lines by `id`; skips non-JSON lines; worker stderr appended (not truncated) to `log_path`; worker exit/EOF fails every in-flight job with `transport: true` and the next job respawns; no message for `inactivity_timeout` → send cancel, then fail the job; no jobs for `idle_timeout` → close stdin and reap the process; `restart_when_idle` does the same as soon as no job is in flight.

### player::queue (T3) — pure, no I/O

- `pub struct Queue` with `new() -> Queue`; `start(context: ContextRef, order: Vec<String>, start_id: &str)`; `current() -> Option<&str>`; `context() -> Option<&ContextRef>`; `set_loop(mode)` / `loop_mode()`; `peek_next_auto() -> Option<String>` (what plays after natural end: `Single` → current; `All`/`Shuffle` → wraps; `None` → `None` after the last); `advance_auto() -> Option<String>`; `next_manual() -> Option<String>` (ignores `Single`; wraps only in `All`/`Shuffle`); `prev_manual() -> Option<String>` (previous in order; wraps only in `All`/`Shuffle`; `None` at the start otherwise); `play_next(id)` (FIFO queue consumed before the order by both auto and manual advance); `remove(id) -> bool` (removes from order and play-next; if it was current, `current()` becomes `None` and the next advance continues from the removed slot); `replace_order(order: Vec<String>)` (context content changed; keeps current and its position by id).
- Shuffle: on `start` or `set_loop(Shuffle)` the order is permuted with the current track first; on wrap it reshuffles; shuffle uses `rand` with an injectable seed (`Queue::with_seed(u64)` for tests).

### player::mpv (T10)

- `pub struct MpvOptions { pub socket_path: PathBuf, pub ao: Option<String>, pub volume: f64, pub extra_args: Vec<String> }`.
- `pub enum MpvEvent { PropertyChange { name: String, data: Value }, EndFile { reason: String }, FileLoaded, PlaybackRestart, Seek, Exited }`, delivered as `(generation: u64, MpvEvent)`.
- `pub trait MpvApi { async fn command(&mut self, args: Vec<Value>) -> anyhow::Result<Value>; async fn quit(&mut self); fn alive(&self) -> bool; }` (native `async fn` in trait; no `async-trait` crate).
- `pub struct MpvProcess` implements it: `spawn(opts: &MpvOptions, generation: u64, events: mpsc::UnboundedSender<(u64, MpvEvent)>) -> anyhow::Result<MpvProcess>`; args `--idle=yes --no-video --no-terminal --input-ipc-server=<sock> --input-media-keys=no --demuxer-max-bytes=32MiB --demuxer-max-back-bytes=8MiB --prefetch-playlist=yes --volume=<v>` (+ `--ao=<ao>` when set, + extra_args); connects with the existing retry loop; a reader task parses lines (extend the existing pure `classify_mpv_line` to return property name/data and end-file reason), resolves replies by `request_id` through a shared pending map (`Arc<Mutex<HashMap<u64, oneshot::Sender<…>>>>`), forwards events, and sends `Exited` on EOF; after connecting it issues `observe_property` for `pause`, `duration`, `volume`, `idle-active`, `path`, `playlist-pos`; command timeout 2 s.
- `pub trait MpvSpawner { type Mpv: MpvApi; async fn spawn(&self, volume: f64, generation: u64, events: mpsc::UnboundedSender<(u64, MpvEvent)>) -> anyhow::Result<Self::Mpv>; }` with `pub struct ProcessSpawner { pub opts_template: MpvOptions }`.

### player::engine (T11)

- `pub trait TrackResolver { fn playable_path(&self, track_id: &str) -> Option<String>; }` (core implements: complete → audio_path; downloading with a known path → `appending://<path>`; else `None`).
- `pub struct Engine<S: MpvSpawner>` with: `new(spawner: S, events: mpsc::UnboundedSender<(u64, MpvEvent)>, volume: f64, loop_mode: LoopMode)`; `async fn play(&mut self, context: ContextRef, order: Vec<String>, start_id: &str, r: &impl TrackResolver) -> Result<(), EngineError>`; `pause`, `resume`, `toggle`, `stop`, `next(r)`, `prev(r)`, `seek(position)`, `set_volume(v)`, `set_loop(mode, r)`, `play_next(id, r)`, `remove_track(id, r)` (if current: advance or stop), `replace_order(order, r)`; `async fn on_mpv_event(&mut self, gen: u64, ev: MpvEvent, r) -> bool` (true = player state changed); `fn idle_deadline(&self) -> Option<tokio::time::Instant>`; `async fn on_idle_deadline(&mut self)`; `fn state(&self) -> PlayerState`.
- Behavior: one mpv process, spawned lazily (generation increments per spawn; events from older generations are ignored); `play` = `loadfile <path> replace` then, once `FileLoaded`, `playlist-clear` and `loadfile <next> append` for `queue.peek_next_auto()` when resolvable (gapless preload); `Single` sets mpv `loop-file=inf`, other modes `no`; on the `path` property changing to the preloaded next path → `queue.advance_auto()`, update current, and preload the following; `idle-active=true` with nothing preloaded → state stopped; `EndFile{reason: "error"}` → skip to next resolvable track; unresolvable ids are skipped when advancing; `time-pos` is never observed; the engine queries `time-pos` once on `PlaybackRestart`, `Seek`, and pause changes and records `(position, at_ms)`; `prev` restarts the current track when position > 3 s; idle deadline armed 10 min after entering paused/stopped, cleared on play/resume; on deadline: record resume point `(track_id, position)`, `quit`, drop the process; `resume` after that respawns and loads the resume point with per-file option `start=<pos>` (loadfile 4th argument; 3rd argument `-1`); mpv `Exited` unexpectedly → state stopped, next command respawns.
- `pub enum EngineError { Unplayable(String), Mpv(anyhow::Error) }`.

### service (T12–T13)

- `pub struct ServeOptions { pub config: Config, pub socket_path: PathBuf, pub state_dir: PathBuf }`; `pub async fn serve(opts: ServeOptions) -> anyhow::Result<()>`.
- Paths: `Config::state_dir()` = `dirs::config_dir()/yplayer` (macOS: `~/Library/Application Support/yplayer`, created 0700); default socket `<state_dir>/yplay.sock` (0600); env `YPLAY_SOCKET` overrides for both serve and client; worker log `<state_dir>/worker.log`; update stamp `<state_dir>/ytdlp_update_check`; session state stays `<cache>/.yplayer_state.json` with fields `volume, loop_mode, last_track_id, last_position, last_context` (all optional, old files still parse).
- Core actor owns `Db`, `Engine<ProcessSpawner>`, `WorkerHandle`, the download table, `broadcast::Sender<Event>` (capacity 1024), `library_version`, and `pending_play: Option<String>`; all mutations happen in the core loop; connection tasks talk to it over an mpsc with oneshot replies.

## Tasks

### T1 — Foundation: rename crate, delete TUI, lib+bin split, final types (wave 0)

- Runs in the main checkout (not a worktree).
- Files: `git mv crates/yplayer-tui crates/yplayer`; root `Cargo.toml` members; `crates/yplayer/Cargo.toml` (package `yplayer`, `[lib] path = "src/lib.rs"`, bin `yplay`); new `src/lib.rs` (registers `config`, `types`, `lyrics`, `library`, `download`, `player` with `pub mod mpv;`); `src/main.rs`; `src/types.rs`; new `src/lyrics.rs`; `src/config.rs`; `src/download/bridge.rs`; `src/download/worker.rs`; delete `src/app.rs`, `src/events.rs`, `src/ui/`, `src/cache/scanner.rs`; move `src/cache/index.rs` → `src/library/db.rs` (rename `CacheIndex` → `Db`, keep behavior) with `src/library/mod.rs`; `.github/workflows/ci.yml` only if it references the old path; `justfile` only if it references the old path.
- Remove dependencies: `ratatui`, `crossterm`, `futures-util`, `fuzzy-matcher`, `unicode-width`, `walkdir`. Switch tokio `rt-multi-thread` → `rt` and add `signal`. Keep `rand`, `which`, `dirs`, `toml`, `clap`, `serde*`, `rusqlite`, `anyhow`.
- `types.rs`: replace with the contract above (`Track` gains `state`, `thumb_path`; `Album` becomes the contract shape; delete `SortMode`, `ViewMode`, `LoopMode::toggle/status_text`); fix `library/db.rs` row mapping to fill `state: TrackState::Complete`, `thumb_path: None` and adapt `list_albums` to return `track_ids` (ordered by position) — no schema change yet (T4 migrates).
- `lyrics.rs`: move `parse_lrc`, `parse_lrc_time` and the `parse_lrc_sorts_and_skips_metadata` test from `app.rs`.
- `config.rs`: drop `SortMode` from `SessionState`; add `loop_mode: Option<LoopMode>`, `last_position: Option<f64>`, `last_context: Option<serde_json::Value>`; keep old files parsing (`#[serde(default)]`, unknown fields ignored); drop `format`, `native`, `embed_meta`, `audio_quality`, `player` from `Config`/`FileConfig` usage (keep `FileConfig` tolerant of those keys in existing files); add `pub fn state_dir() -> PathBuf`.
- `download/bridge.rs`: remove `download` and `lyrics` methods and `WorkerCommand` fields they alone used; keep `new`, handshake, `send`, `search`, `list_formats`, `playlist_entries`, `worker_command`, `find_python`; worker stderr log opened in append mode.
- `download/worker.rs`: delete the file (T9 recreates it); remove it from `download/mod.rs`.
- `main.rs`: `#[tokio::main(flavor = "current_thread")]`; clap subcommands `search <query> [--limit N]` and `formats <url>` using `Bridge` (same output format as today's `print_search_results` / list-formats); no TUI, no default action (prints help).
- Tests: existing db tests updated to new types; `session_state_round_trips` updated; new test `old_session_file_with_sort_mode_still_parses`; `parse_lrc` test moved; delete tests of removed code.
- Acceptance: all gates pass; `cargo build --release` succeeds; `./target/release/yplay --help` lists `search` and `formats`; `cargo tree -p yplayer | grep -E "ratatui|crossterm"` is empty.
- Commit: `SP1 T1: rename crate, delete TUI, lib+bin split, final core types`.

### T2 — Protocol + ytid (wave 1, agent A)

- Files: `src/protocol.rs`, `src/ytid.rs`, `src/lib.rs` (module lines).
- Tests first (`protocol.rs`): request round-trip for every `Command` variant using the exact wire JSON from the contract (one table-driven test), `ContextRef`/`AlbumRef` untagged shapes, `add` default `play: true`, `track.delete` default `to_trash: true`, `decode_request` on invalid JSON → `bad_request` with id 0, on unknown cmd with id 7 → `unknown_command` with id 7, `Event` serialization for every variant (exact JSON strings), `PlayerState` uses the key `loop`, `Response::ok` omits `error` and `Response::err` omits `result`, `encode_line` ends with exactly one `\n`.
- Tests first (`ytid.rs`): accepts each accepted form (with/without scheme, `www.`/`m.`/`music.`, extra params before/after `v`, `&list=` present, `youtu.be/ID?si=x`, shorts, live, embed, bare id); rejects `youtube.com/playlist?list=PL…` and `watch?list=…` as `PlaylistOnly`; `vimeo.com/123` and `notyoutube.com/watch?v=…` as `NotYouTube`; `watch?v=short` as `NoVideoId`.
- Acceptance: gates pass.
- Commit: `SP1 T2: socket protocol v1 types and YouTube URL parsing`.

### T3 — Queue (wave 1, agent A, after T2)

- Files: `src/player/mod.rs` (add `pub mod queue;`; T1 already registers `player` with `pub mod mpv;`), `src/player/queue.rs`.
- Tests first: start at middle of order; `None` mode stops after last on auto advance and on manual next; `All` wraps both ways; `Single` auto repeats current but manual next moves on; play-next items come before the order and are consumed once; `prev_manual` at start returns `None` in `None` mode and wraps in `All`; `remove` of current then `advance_auto` continues with the item after the removed one; `remove` of a non-current item keeps current; `replace_order` keeps current by id (and its new position); shuffle with a fixed seed puts current first, visits every id exactly once per cycle, reshuffles on wrap; drop-play order `[dropped] + others` behaves as a normal order.
- Acceptance: gates pass.
- Commit: `SP1 T3: pure playback queue with loop, shuffle and play-next`.

### T4 — Database migration and library ops (wave 1, agent B)

- Files: `src/library/db.rs`, `src/types.rs` (add `TrackMeta`), `src/lib.rs` only if needed.
- Tests first: fresh DB ends at `user_version = 1` with all new columns/table/index; a fixture v0 DB (create with the old DDL in the test, insert rows) migrates preserving rows and defaults `state = complete`; migration imports `albums/*.album.json` once (second `open` does not re-add a link removed after the first open); `insert_pending` then `mark_started` then `mark_complete` transitions and fields; `insert_pending` on an existing failed row keeps title/added_at; `list_tracks` order (`added_at DESC, title NOCASE`); album create/duplicate → `Conflict`; rename conflict; `album_add` appends and is idempotent; `album_remove`; `album_reorder` rewrites order and rejects non-permutations; `albums_containing`; `last_used_album` (none used → `None`; ties); deleting a track cascades album links and lyrics; `put_lyrics`/`get_lyrics` including miss rows; CJK title round-trip test kept.
- Acceptance: gates pass.
- Commit: `SP1 T4: schema v1 migration, album editing and lyrics cache in SQLite`.

### T5 — Reconcile (wave 1, agent B, after T4)

- Files: `src/library/reconcile.rs`, `src/library/mod.rs`.
- Tests first (temp dirs): imports an unknown per-track folder with `meta.json`, `audio.webm`, `cover.webp` (sets `thumb_path`, `format = "webm"`, `file_size`); ignores a folder already referenced by a row; imports a legacy flat `<id>.mp3` with and without `<id>.json`; leaves an unknown dir without `meta.json` and reports it in `skipped_dirs`; deletes a partial `Title [abcdefgh]` dir without `meta.json` when a `failed` row has id prefix `abcdefgh` and is not in `in_flight`, but keeps it when the id is in `in_flight`; never deletes a dir containing `meta.json`; removes `complete` rows whose file vanished and keeps `downloading` rows; `recover_interrupted` fails `downloading` rows and deletes their `[id8]` dir.
- Acceptance: gates pass; reconcile on a synthetic 5,000-folder library where every folder is already known completes in < 50 ms in release mode (add an `#[ignore]` benchmark-style test that prints the elapsed time; report the number).
- Commit: `SP1 T5: background reconcile replaces the startup scan`.

### T6 — Python worker v2 (wave 1, agent C)

- Files: `yplayer/worker.py`, `yplayer/core.py`, `pyproject.toml`, `tests/test_worker_smoke.py`, new `tests/test_worker_protocol.py`, `pyproject.toml` pytest config for the `live` marker.
- `core.py`: add `download_track(url, cache_dir, *, emit, cancel_event) -> dict` implementing the spec's drop-flow step 5 and the worker protocol v2 (`format: "bestaudio"`, `nopart: True`, `writethumbnail: True`, outtmpl dict with `default` and `thumbnail` keys under `<cache>/%(title).150B [%(id).8s]/`, `quiet`, `no_warnings`, `logtostderr`, `noprogress`, `noplaylist`, `retries: 2`, `socket_timeout: 10`, `progress_hooks` that emit `started` once and throttled `progress`, raise `yt_dlp.utils.DownloadCancelled` when `cancel_event` is set); after success write `meta.json` (`id, title, uploader, duration, webpage_url`) last and return `{path, dir, meta, thumb, format, file_size}`; delete `download_audio`, `_ydl_extract`'s auto-update retry (callers use `YoutubeDL` directly), `_auto_update_ytdlp`, `_update_ytdlp_pip`, `ensure_ytdlp_uptodate`, all lyrics code, `save_sidecar`'s legacy flat `<id>.json` write, and any helper left unused by these removals (keep search/list_formats/playlist helpers).
- `worker.py`: protocol v2 exactly as the contract (ready with `protocol: 2`, per-request explicit ids — remove the `_current_id` global, locked `_respond`, `ThreadPoolExecutor(max_workers=3)`, inline `cancel`, EOF → wait for jobs → exit 0); no background update thread; remove `video_info` and `lyrics` handlers.
- `pyproject.toml`: dependency `yt-dlp[default]`; install the extras with `uv pip install --python /Users/haowenchen/Files/projects/yplayer/.venv/bin/python "yt-dlp[default]"` (not `-e .`).
- Tests first (`tests/test_worker_protocol.py`, with `yt_dlp.YoutubeDL` replaced by a fake via monkeypatch in an in-process harness that runs the worker's dispatch with fake stdin/stdout): ready line has `protocol: 2`; download emits `started` exactly once then progress (≤2/s using an injectable clock) then terminal `ok` with `meta.json` present on disk and written after the audio; cancel during progress yields `ok: false, cancelled: true` and the cancel request's own `ok: true`; three concurrent downloads interleave and each terminal carries its own id; a yt-dlp `DownloadError` yields `ok: false` with its message and does not run pip (assert no subprocess call); unknown command yields `unknown` error with id; EOF with a running job waits for it. Keep `test_worker_protocol_roundtrip` in the smoke test updated to expect `protocol: 2`.
- Live test (`@pytest.mark.live`, skipped by default): real download of `https://www.youtube.com/watch?v=jNQXAC9IVRw` into a temp dir yields `audio.webm` or `audio.m4a`, `cover.*`, `meta.json`, and a `started` event before completion.
- Acceptance: ruff + pytest gates pass; run the live test once and report its timing.
- Commit: `SP1 T6: worker protocol v2 — concurrent, streamed, cancellable native downloads`.

### T7 — HTTP client + Rust lyrics (wave 1, agent D)

- Files: `src/http.rs`, `src/lyrics.rs`, `src/lib.rs`, `crates/yplayer/Cargo.toml` (add `regex-lite`).
- Tests first: `clean_track_title` table test whose expected values are produced by running the Python `_clean_track_title` on the same inputs (include at least: `ずっと真夜中でいいのに。『秒針を噛む』MV`, `ZUTOMAYO「ミラボ」Official Video`, `Song Name (Official Music Video)`, `Artist - Title [Lyric Video]`, `Title feat. Someone`, `【MV】Title`, plain `Title`); list any input where Rust and Python differ and why in the commit message; `fetch` with a fake `HttpGet`: `/api/get` hit → Found synced without calling search; `/api/get` 404 then search hit with closest duration chosen; duplicate candidates are requested once; plain-only → Found `synced: false`; 429 → Error and stops; all empty → Missing; artist or duration missing → skips `/api/get`; URL query parameters are percent-encoded (CJK). `CurlHttp` gets one `#[ignore]` live test against `https://lrclib.net/api/search?track_name=ham&artist_name=zutomayo`.
- Acceptance: gates pass.
- Commit: `SP1 T7: curl-backed HTTP client and LRCLIB lyrics in Rust`.

### T8 — yt-dlp updater (wave 1, agent D, after T7)

- Files: `src/updater.rs`, `src/lib.rs`.
- Tests first: `parse_version`/`is_newer` (`2026.06.09` vs `2026.06.09.1`, vs `2026.10.01`, equal, garbage); `version_from_location` on `https://github.com/yt-dlp/yt-dlp/releases/tag/2026.06.09`; `due` with missing stamp, fresh stamp, 25 h-old stamp (set mtime via `std::fs::File::set_modified`); `check_and_upgrade` with fake http/runner: newer → runs exactly `uv pip install --python <py> -U yt-dlp[default]` and returns `Upgraded`; same → `UpToDate` and touches stamp; uv missing → `Skipped`; runner failure → `Failed` and stamp untouched.
- Acceptance: gates pass.
- Commit: `SP1 T8: daily yt-dlp update check via GitHub redirect and uv`.

### T9 — WorkerHandle v2 (wave 2, agent E)

- Files: `src/download/worker.rs`, `src/download/mod.rs`, `src/download/bridge.rs` (share `worker_command`/`find_python`/spawn+handshake helpers as `pub(crate)` if useful).
- Tests first, with fake Python worker scripts written to a temp dir and injected via `YPLAY_WORKER_CMD` (run these tests serially in one `#[tokio::test]` per scenario group, as today, because the env var is process-global): streamed download maps to `Started` → `Progress` → `Done`; two concurrent jobs route by id; `cancel` produces `Failed { cancelled: true }`; worker crash mid-job → `Failed { transport: true }` and the next job respawns a new process (count spawns via a file the fake appends to); idle timeout (set 200 ms in the test) closes stdin and the fake exits; inactivity timeout (300 ms) fails a silent job; stderr is appended to `log_path` across two spawns.
- Acceptance: gates pass.
- Commit: `SP1 T9: on-demand concurrent worker handle with streaming, cancel and idle exit`.

### T10 — mpv process v2 (wave 2, agent F)

- Files: `src/player/mpv.rs`, test fixture `crates/yplayer/tests/fixtures/tone.opus` (generate once: `ffmpeg -f lavfi -i "sine=frequency=440:duration=4" -c:a libopus -b:a 24k tone.opus`; commit the file, < 20 KB) plus a second 4 s fixture `tone2.opus` at 660 Hz.
- Tests first (pure): extended `classify_mpv_line` for `property-change` (name + data), `end-file` (reason), `file-loaded`, `playback-restart`, `seek`, replies; existing tests kept.
- `#[ignore]` integration tests with real mpv (`--ao=null` via `extra_args`): spawn → `command(["get_property","idle-active"])` is true; `loadfile` fixture → receive `FileLoaded` and `PropertyChange{duration}`; `quit` → `Exited`; command after exit errors quickly (< 2 s).
- Acceptance: gates pass; the ignored tests pass locally (`cargo test -p yplayer -- --ignored mpv`), reported.
- Commit: `SP1 T10: persistent event-driven mpv process`.

### T11 — Playback engine (wave 2, agent F, after T10)

- Files: `src/player/engine.rs`, `src/player/mod.rs`.
- Tests first with a fake `MpvSpawner`/`MpvApi` that records commands and lets the test inject events: `play` issues `loadfile <path> replace` then after `FileLoaded` issues `playlist-clear` + `loadfile <next> append`; `path` change to the preloaded next advances the queue and preloads the following; `Single` sets `loop-file=inf`; `None` mode with last track → `idle-active` → stopped; unresolvable next is skipped; `prev` within 3 s goes to previous, after 3 s seeks to 0; `remove_track(current)` advances; `EndFile{error}` skips; events from an old generation are ignored; `time-pos` queried only on `PlaybackRestart`/`Seek`/pause change (assert command log never contains `observe_property time-pos`); idle deadline set 10 min after pause and cleared on resume; `on_idle_deadline` quits and a later `resume` respawns and loads with `start=<pos>` (assert loadfile args `[path, "replace", -1, "start=<pos>"]`); unexpected `Exited` → stopped and next `play` respawns (generation + 1).
- `#[ignore]` integration test with real mpv (`--ao=null`) and the two fixtures: play both in order, observe the transition via the `path` property without a new process spawn (assert one spawn), then stop.
- Acceptance: gates pass; ignored engine test passes locally, reported.
- Commit: `SP1 T11: gapless event-driven playback engine with idle shutdown`.

### T12 — Service core and socket server (wave 3)

- Files: `src/service/mod.rs`, `src/service/core.rs`, `src/service/conn.rs`, `src/config.rs` (paths), `src/main.rs` (`serve` subcommand: `yplay serve [--dir <cache>] [--socket <path>]`), `src/lib.rs`.
- mpv spawning: `ProcessSpawner` built from config; env `YPLAY_MPV_EXTRA_ARGS` (whitespace-split) is appended to mpv args (tests pass `--ao=null`).
- Startup: create state dir 0700; single instance (connecting to an existing socket succeeds → exit with error "already running"; otherwise remove the stale file); bind; chmod 0600; `Db::open`; `recover_interrupted`; restore session (volume, loop; no autoplay); spawn reconcile on `spawn_blocking` with its own `Db` connection, then emit `track.upsert`/`track.removed` for the report and bump `library_version`; if the updater is `due`, run it on `spawn_blocking` when the worker is not `busy()`, then `restart_when_idle` on `Upgraded`; re-check 24 h later (one `sleep`); SIGTERM/SIGINT → save session, `quit` mpv, remove socket, exit 0.
- Commands implemented here: `hello` (mismatched protocol → `protocol_mismatch`), `subscribe`, `library.get`, `now`, `play` (album context order from `get_album`; library context from `library_order`; `touch_last_played`; `touch_album` for album contexts), `pause`, `resume`, `toggle`, `stop`, `next`, `prev`, `seek`, `volume`, `loop`, `queue.play_next`, all `album.*` (emit `album.upsert`/`album.removed`; call `engine.replace_order` when the playing context's album changes), `track.rename`, `rescan`, `lyrics` (DB cache first; miss rows younger than 7 days → `{missing: true}`; else fetch on `spawn_blocking` with `CurlHttp`, store Found/Missing, never store Error; concurrent requests for one track share one fetch; results for tracks no longer requested are still cached).
- Player events: emit `player` whenever `engine.on_mpv_event` returns true or a command changes state; save session state on track change, pause and stop.
- Connections: line reader enforcing `MAX_LINE` (oversized → `bad_request` then close); per-connection writer task; `subscribe` attaches a broadcast receiver; `Lagged` → send `resync`.
- Tests (in-process, temp cache dir, temp socket path, fake `MpvSpawner` from T11 tests exposed under `#[cfg(test)]` or a `testing` module, fake worker via `YPLAY_WORKER_CMD`): hello/subscribe/library.get shapes; album create/rename/add/remove/reorder/delete emit the right events and `library_version` increments; play album context emits `player` with context; second instance refuses to start; stale socket file is replaced; oversized line → `bad_request` and close; lagging subscriber gets `resync` (use a tiny broadcast capacity in a test constructor); SIGTERM path saves session (call the shutdown function directly).
- Acceptance: gates pass; `yplay serve --dir <tmp>` runs and `nc -U <sock>` with `{"id":1,"cmd":"hello","protocol":1}` answers (report the exchange).
- Commit: `SP1 T12: yplay serve — core actor, socket server, playback and album commands`.

### T13 — Add, delete, retry flows (wave 3, after T12)

- Files: `src/service/downloads.rs`, `src/service/core.rs`, `src/service/mod.rs`.
- `add`: `ytid::parse_video_id` (`NotYouTube`/`NoVideoId` → `invalid_url`, `PlaylistOnly` → `unsupported_url` "Playlists are not supported yet"); resolve album (`None` → `last_used_album` or `get_or_create_album("Inbox")`; `Name` → get-or-create; `Id` → must exist else `not_found`); `touch_album`; cache hit (row `complete` and file exists) → `album_add`, and if `play` → play context `[id] + album others`; already downloading → `album_add`, and if `play` set `pending_play` (play immediately via `appending://` if `started` already arrived); otherwise `insert_pending`, `album_add`, `WorkerHandle::download`, forward the job's messages into the core, emit `download {phase: fetching}`, set `pending_play` when `play`; reply `AddResult`.
- Worker messages: `Started` → `mark_started`, emit `track.upsert`, `download {downloading}`, and if `pending_play == id` → play context `[id] + others of the album from the originating add` with the `appending://` path and clear `pending_play`; `Progress` → `download` event throttled to ≤ 2/s per track; `Done` → `mark_complete`, emit `track.upsert` + `download {done}`; `Failed` → cancelled: delete the dir if known, emit `download {cancelled}`; otherwise `mark_failed`, delete the dir if known, emit `track.upsert` + `download {failed, error}` + error toast; clear `pending_play` if it matches; `transport: true` failures toast "Downloader unavailable: …".
- `track.delete`: cancel an in-flight job (remove the dir after its terminal message); `engine.remove_track`; `delete_track` (cascade); files: per-track dir (parent of `audio_path` whose name ends with `[<id8>]`) or legacy flat audio file + `<id>.json`; `to_trash: true` → `trash` crate with the macOS NSFileManager delete method (`trash::macos::DeleteMethod::NsFileManager` via `TrashContext`; never the Finder/AppleScript method), `false` → `remove_dir_all`/`remove_file`; emit `track.removed` and `album.upsert` for every album that contained it.
- `track.retry`: only for `failed` rows (else `bad_request`); same path as a new download without album changes and without play.
- Add dependency `trash` (latest), gated on nothing (it supports macOS and Linux).
- Tests (fake worker scripts + fake mpv): cache hit plays with zero worker spawns (assert via spawn-count file); new URL → `fetching` → `started` → engine played `appending://<path>` → `done` and row complete; dropping a second URL while the first downloads starts the second immediately on its `started` (concurrency); playlist URL → `unsupported_url`; non-YouTube → `invalid_url`; undo flow (`album.remove` + `track.delete {to_trash: false}`) during download cancels the job and removes the dir; failure deletes the dir, marks failed, toasts; retry re-downloads; delete of the current track advances playback; `to_trash: true` moves the folder to the trash (test uses a temp dir and asserts the source is gone; `#[ignore]` on CI-hostile environments only if the trash call fails in the sandbox, and say so).
- Acceptance: gates pass.
- Commit: `SP1 T13: add/delete/retry flows with stream-into-cache playback`.

### T14 — CLI client (wave 3, after T13)

- Files: `src/client.rs`, `src/main.rs`, `src/lib.rs`, `crates/yplayer/tests/cli_e2e.rs`.
- Subcommands: `serve` (from T12), `add <url> [--album NAME] [--no-play] [--wait]`, `play <track_id> [--album NAME]`, `pause`, `resume`, `toggle`, `stop`, `next`, `prev`, `now`, `albums`, `search <query> [--limit N]`, `formats <url>`; global `--socket <path>` (else `YPLAY_SOCKET`, else default).
- Output: `add` prints `Added <title-or-id> → <album>`; with `--wait` subscribes before sending `add`, then prints `first audio after <x.x>s` on the `started`-driven `player` event for that track (or `cached — playing` on a cache hit) and `downloaded in <y.y>s` on `done`, exit 1 on `failed`; `now` prints `▶ Title — Uploader  m:ss / m:ss  [album]` (or `⏸`/`■`); `albums` prints `name  (n songs)` per line.
- Exit codes: 0 ok; 1 service returned an error (print its message); 2 cannot connect (print `yplay service is not running — start it with: yplay serve`).
- E2E test (`tests/cli_e2e.rs`, uses the built binary via `env!("CARGO_BIN_EXE_yplay")`, a temp cache/socket, fake worker via `YPLAY_WORKER_CMD`, and `--ao=null` through an env var `YPLAY_MPV_EXTRA_ARGS` that the service appends to mpv args; `#[ignore]` because it needs mpv): `serve` in background, `add <url> --wait` succeeds, second `add` of the same URL reports `cached`, `now` prints the track, `albums` lists Inbox, killing the server with SIGTERM removes the socket.
- Acceptance: gates pass; ignored e2e passes locally, reported.
- Commit: `SP1 T14: yplay CLI client`.

### T15 — Install, perf script, docs (wave 4)

- Files: `packaging/com.yplayer.service.plist.in`, `scripts/install.sh`, `scripts/uninstall.sh`, `scripts/perf-budget.sh`, `justfile`, `README.md`, `.github/workflows/ci.yml`.
- `install.sh`: `cargo build --release`; copy `target/release/yplay` to `~/.local/bin/yplay`; `uv pip install --python .venv/bin/python -e .`; ensure config.toml has `worker_python = "<abs repo>/.venv/bin/python3"` (add only if the key is absent; create the file if missing); render the plist (ProgramArguments `~/.local/bin/yplay serve`; `KeepAlive` true; `RunAtLoad` true; `EnvironmentVariables.PATH` = `/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin`; `StandardOutPath`/`StandardErrorPath` = `~/Library/Logs/yplayer/service.log`; `ProcessType` = `Interactive`); `launchctl bootout gui/$UID/com.yplayer.service` (ignore failure) then `launchctl bootstrap gui/$UID <plist>`; print status. Idempotent. `uninstall.sh` reverses (keeps cache, DB, config).
- `perf-budget.sh`: finds the `yplay serve` pid (and mpv/python children); samples `top -l 21 -s 1 -stats pid,command,cpu,idlew,mem,threads` for a given state label; prints average CPU, idle wakeups/s and max memory per process and PASS/FAIL against the spec's service rows (`idle`: service 0 idle wakeups/s (allow ≤ 0.2), < 10 MB, no mpv/python; `playing`: mpv ≤ 60 MB and service ≤ 0.2 wakeups/s).
- `justfile`: `install`, `uninstall`, `perf STATE`, `e2e` (runs ignored tests) call the scripts; update `check-python` to use `.venv/bin/ruff` and `.venv/bin/python -m pytest`.
- `README.md`: replace TUI usage with the service + CLI usage, install steps, the dependency table (mpv required; ffmpeg no longer required; uv + deno used by the worker).
- CI: update paths if needed; the Python job installs with `pip install .[dev]` today — keep it working with `yt-dlp[default]`.
- Acceptance: gates pass; `bash -n` on every script; `scripts/install.sh` run for real on this machine; `launchctl print gui/$UID/com.yplayer.service` shows running; report outputs.
- Commit: `SP1 T15: LaunchAgent install, perf budget script, README`.

### T16 — Verification (supervisor, not delegated)

- Real service via LaunchAgent; test URL `https://www.youtube.com/watch?v=jNQXAC9IVRw` plus one real song.
- `yplay add <url> --wait`: first audio ≤ 4 s; second add: `cached`, zero network (worker not spawned: check `ps` and `worker.log`).
- `scripts/perf-budget.sh idle` after 10 min stopped (mpv gone, worker gone), `playing` while a track plays; compare with the audit baseline in the spec.
- Gapless: play an album of two real tracks; confirm a single mpv pid across the transition and no audible gap/cut at the end (listen once; record `end-file` and `path` timing from the service log).
- Delete to Trash (file appears in `~/.Trash`), undo during download (folder gone), failure path (private/unavailable video id) toasts and does not touch pip.
- Existing library: all previous tracks present after migration; albums preserved.
- Update the spec's verification items with outcomes; update memory `native-app-project`.
