# Hardening plan (after the 2026-09-29 safety, robustness and performance reviews)

Branch `native-app`, base fee9d01. Four fix tracks run in parallel worktrees; each owns the files listed and keeps edits to `crates/yplayer/src/service/core.rs` minimal and inside the functions named, so merges stay small. Review evidence and reproduction scripts: session scratchpad `review-sec/`, `review-rob/`, `review-perf/`.

## Rules (all tracks)

- Test first for every item that names a test; tests never touch `~/Music/yt-audio`, the installed service socket, launchd or the running apps; temp sockets under `/tmp` (< 104 bytes).
- Gates: Rust (`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`), Python (`.venv/bin/ruff check yplayer/ tests/`, `.venv/bin/python -m pytest -q`), Swift (`scripts/swift-test.sh`, `swift build -c release --package-path apps/macos -Xswiftc -warnings-as-errors`, `swift format lint --strict --recursive apps/macos/Sources apps/macos/Tests`) — run the ones for languages you touched.
- One commit per track: `Hardening <track>: …`, ending with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Track A — files, deletes, library (Rust)

Owns: `library/reconcile.rs`, `library/db.rs`, new `library/safe_fs.rs`, `service/downloads.rs`, `service/mod.rs` (startup), `config.rs`; in `core.rs` only the delete/cleanup call sites, `on_reconciled`, and the album/track name handlers.

- A1 (S3, L1, H2, L6-service): one `safe_fs::remove_track_dir(cache_dir, dir, track_id, mode)` used by every folder delete (failure cleanup, cancel cleanup, `track.delete`, reconcile partial cleanup, `recover_interrupted`). It deletes only when: the canonical path's parent is the canonical `cache_dir`; the folder name ends with `[<first 8 chars of track_id>]`; it is not a symlink. For automatic cleanups (not an explicit user delete) it additionally requires that the folder holds only files the pipeline creates (`audio.*`, `cover.{jpg,png,webp}`, `*.tmp`, `*.part`, `*.ytdl`, `meta.json`) — any other file means leave it and log. Tests: cache root, a folder outside the cache, a symlink, a wrong suffix, a folder with a user file → all refused; a normal partial and an explicit delete → removed.
- A2 (H2): a cancelled job's folder is not removed when the same track is downloading again or exists again in the DB (re-added after Undo). Test: undo then re-add the same URL while the old job is still ending → the new download completes.
- A3 (S2): reconcile imports only ids matching `[A-Za-z0-9_-]{11}` (others are logged and skipped); the legacy sidecar deleted with a flat track is `<audio stem>.json` next to the audio file, and it is only deleted if inside `cache_dir`. Test: a meta.json with id `../../x` is skipped; deleting a legacy track never touches files outside the cache.
- A4 (S1): every temp file the service writes (`cover.<ext>.tmp`) is created with `create_new` (fails on an existing file or symlink). Test: a planted `cover.png.tmp` symlink is not followed.
- A5 (H3): reconcile and `recover_interrupted` handle each folder independently — an error on one is logged (with its path) and the scan continues; startup never exits because of a single folder. Test: an undeletable partial folder does not stop the scan or startup.
- A6 (M6): reconcile relinks instead of deleting: when an unknown folder's `meta.json` id matches a row whose audio file is missing, update that row's `audio_path`/`thumb_path` (album links kept). Test: rename a track folder, rescan → same row, same albums, new path.
- A7 (M5): if the DB cannot be opened because it is not a valid database (not "locked"), move it to `.yplayer.db.corrupt-<unix time>`, create a fresh one, let reconcile rebuild tracks from the folders, and emit a warn toast after startup ("Your library database was damaged and has been rebuilt from your music folder; albums could not be recovered."). Open the DB before binding the socket. Test: a garbage `.yplayer.db` → service starts, tracks rebuilt, corrupt file kept.
- A8 (I1, I2): the service sets `umask(0o077)` at startup (DB, session file, logs and the mpv socket are created owner-only).
- A9 (L8): the session file is written to a temp file and renamed.
- A10 (P8): when a reconcile imports or relinks more than 200 tracks, emit one `resync` instead of one event per track.
- A11 (P10): a checked-and-no-cover track stores `thumb_path = ''` (exposed as `None`, like `audio_path`), and reconcile skips tracks whose `thumb_path` is `''`.
- A12 (S5 server side): `album.create`/`album.rename` names are trimmed and limited to 1..=200 characters, `track.rename` titles to 1..=500; otherwise `bad_request`.

## Track B — player and connections (Rust)

Owns: `player/mpv.rs`, `player/engine.rs`, `service/conn.rs`; in `core.rs` only the playback command handlers and the download-failure → engine hook.

- B1 (H1): before spawning mpv, if the mpv socket path exists and accepts a connection, send `quit` to that old mpv and wait up to 1 s; then remove the socket. Test with a real mpv (`#[ignore]`): an orphan mpv from a killed service is gone after the next spawn.
- B2 (I3): mpv also gets `--no-config --load-scripts=no --ytdl=no` (keep `--audio-format=float`, `--input-media-keys=no`, `--media-controls=no`).
- B3 (M3): two consecutive command timeouts kill the mpv process (SIGKILL), mark the player stopped with a warn toast ("The player stopped responding and was restarted"), and the next command respawns it; the queue only moves after a successful load. Test with a fake mpv that stops answering.
- B4 (L1-rob): when mpv exits unexpectedly, record the resume point (track + last known position) as idle shutdown does, emit a warn toast, and `resume` continues from that position.
- B5 (L2-rob): when the download of the currently playing (streaming) track fails, the engine skips to the next track (or stops) instead of playing a failed track.
- B6 (L3-rob): `resume`/`toggle`/`next`/`prev` return `player_unavailable` when mpv cannot be spawned (not silent success).
- B7 (L3-sec, M7): `volume` must be finite and is clamped to 0..=100; `seek` must be finite and ≥ 0; non-finite → `bad_request`; engine state (and the session file) change only after mpv accepted the command.
- B8 (L5): each connection has at most 64 requests in flight; it stops reading until replies are written (backpressure). Test: a client that floods 20,000 requests without reading keeps the service under 30 MB.

## Track C — downloads, worker, updater, HTTP (Rust + Python)

Owns: `download/worker.rs`, `download/bridge.rs`, `updater.rs`, `http.rs`, `yplayer/*.py`, `tests/*.py`; in `core.rs` only the `add` path (priority flag) and the use of the new `dir` field on failure.

- C1 (M1, P3): the worker emits `{"id": N, "event": "running"}` when a job starts on a pool thread; the Rust inactivity deadline starts at `running` (a queued job has no deadline). Test: 8 jobs with a 1-slot fake pool and a short inactivity timeout → none fail as stalled.
- C2 (M2): pool size 4; the Rust actor keeps its own queue and sends at most 3 background jobs plus 1 play-requested job to the worker; `WorkerHandle::download` takes `priority: bool` and the service sets it when the add asked to play. Test: with 3 slow jobs running, a priority job starts at once.
- C3 (L4): worker stdout lines longer than 4 MiB are discarded (without buffering them whole) and logged.
- C4 (L6): failed and cancelled terminal replies include `dir` when the job had created its folder (known from the progress hook's filename); `WorkerMsg::Failed` gains `dir: Option<String>`, and the service removes that folder through Track A's `safe_fs` rules (use the same function; if Track A has not merged yet, call a stub with the same signature and note it for the supervisor).
- C5 (L7): yt-dlp options `retries: 1`, `extractor_retries: 1`, `socket_timeout: 8`. Test: options passed to the fake YoutubeDL.
- C6 (L2-sec): the worker is spawned as `python -P -m yplayer.worker` with its working directory set to the state dir; `find_python` no longer searches the current directory.
- C7 (S1-py): `meta.json.tmp` and `cover.jpg` are created with `os.open(..., O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW)` (a leftover temp file is removed first only if it is a regular file, never followed). Test: a planted symlink is not followed.
- C8 (S4): the updater installs exactly the GitHub release tag, binary-only, delayed a day, upgrading only yt-dlp packages: `uv pip install --python <py> --only-binary :all: --exclude-newer <now − 24 h, RFC 3339> --upgrade-package yt-dlp --upgrade-package yt-dlp-ejs "yt-dlp[default]==<tag>"`; it logs the installed versions; a failure (e.g. the release is less than a day old) is `Failed` and retried at the next daily check. Update the updater tests.
- C9 (I4): curl gets `-q --proto =https --max-filesize 5000000`.

## Track D — menu-bar app (Swift)

Owns: `apps/macos/**`.

- D1 (P1, S5): the client accepts incoming lines up to 64 MiB (requests it sends stay small); a failed post-connect `libraryGet` does not reset the reconnect backoff or trigger a reconnect loop. Test: a 5 MB line is accepted by `LineFramer`.
- D2 (P2): `TrackRow`'s body is always a single container view (the conditional content lives inside it). Verify with a 5,000-track snapshot fixture that an upsert costs < 50 ms (measure and report).
- D3 (P4): toasts carry their creation time; the store keeps at most 3, drops an exact duplicate of the newest, and drops toasts older than 10 s when appending; the banner shows only toasts younger than 10 s.
- D4 (P6): search keys are kept in an array aligned with `libraryOrder`; a query that extends the previous query filters the previous results only. Test: 20,000 tracks, per-keystroke search < 5 ms in a release-mode measurement (report the number; assert < 20 ms).
- D5 (P7): `upsert` finds the insert position by binary search on cached sort keys and only reassigns `libraryOrder` when it changed; `load` sorts with precomputed keys. Test: 20,000-track load < 150 ms, upsert < 0.5 ms (report numbers; assert with slack).
- D6 (P11): artwork `NSCache` `countLimit = 300`, `totalCostLimit = 64 MB` (cost = bytes per row image).
- D7 (P13): `load` drops download-progress entries for tracks that are not in state `downloading`.
- D8 (L9): an empty, non-final socket read is treated as end of stream.
- D9 (I5): text drops are accepted only if the string contains a YouTube URL (a bare 11-character word is not a video).
- D10 (I6): "Copy YouTube Link" builds `https://www.youtube.com/watch?v=<id>` from the validated track id.
- D11 (L4-sec): Undo sends the downloaded folder to the Trash (`track.delete` with `to_trash: true`).

## Supervisor after merge

Re-run the reviewers' reproductions for S1, S2, S3, H1, H2, H3, M1, M2, M5, P1, P2 against the merged build; full gates; reinstall; perf rows; record results in the spec.
