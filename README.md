# Yplayer

A YouTube audio player for macOS with a local cache. `yplay serve` runs in the background as a LaunchAgent and owns the library, playback, and downloads; the `yplay` CLI talks to it over a Unix socket. New URLs start playing while they download (first audio in a few seconds), and cached tracks play with no network access.

The terminal UI (Ratatui) has been removed. A SwiftUI menu-bar app is the planned UI; it is upcoming and not available yet. Until then, control playback with the `yplay` CLI.

## Architecture

```
 yplay CLI (add/play/pause/…) ──┐ Unix socket, newline-delimited JSON
                                │ ~/Library/Application Support/yplayer/yplay.sock
                                ▼
                        yplay serve (Rust, LaunchAgent, KeepAlive)
                        ├─ library: SQLite, the only writer
                        ├─ player: one persistent mpv (--idle=yes), event-driven IPC
                        ├─ downloader: on-demand Python yt-dlp worker, concurrent jobs
                        └─ lyrics: LRCLIB over HTTP, cached in SQLite
```

- The service is the single source of truth: clients never touch the DB, mpv, or cache files.
- mpv starts on first playback and exits after 10 minutes stopped; the Python worker starts on the first download and exits 60 s after its last job.
- yt-dlp is checked for updates at most once a day and upgraded with `uv` when a newer release exists.

## Requirements

| Tool | Required | Purpose |
|------|----------|---------|
| **mpv** | Yes | Audio playback |
| **Rust (cargo)** | Yes | Builds the `yplay` binary |
| **Python 3.11+** | Yes | Runs the yt-dlp download worker (in the repo's `.venv`) |
| **uv** | Yes | Installs the worker package; upgrades yt-dlp |
| **deno** | Yes | JavaScript runtime yt-dlp uses for YouTube downloads |
| **ffmpeg** | No | No longer required (audio is stored in its native format) |

```bash
brew install mpv uv deno
```

## Install

```bash
git clone https://github.com/HaoWen46/yplayer.git
cd yplayer
uv venv .venv
scripts/install.sh        # or: just install
```

`scripts/install.sh` is idempotent; re-run it after pulling changes. It:

1. Builds with `cargo build --release` and installs the binary to `~/.local/bin/yplay`.
2. Installs the worker package with `uv pip install --python .venv/bin/python -e .`.
3. Adds `worker_python = "<repo>/.venv/bin/python3"` to `config.toml` only if that key is absent (creating the file if needed); existing values are never changed.
4. Writes `~/Library/LaunchAgents/com.yplayer.service.plist` (runs `~/.local/bin/yplay serve`, `KeepAlive`, `RunAtLoad`, logs to `~/Library/Logs/yplayer/service.log`).
5. Stops the service if it is loaded, then backs up the library DB once: copies `<cache_dir>/.yplayer.db` to `<cache_dir>/.yplayer.db.pre-service.bak` if the DB exists and no backup exists yet (an existing backup is never overwritten).
6. Starts the service with `launchctl bootstrap gui/$UID` and prints its status.

Check the service with `launchctl print gui/$UID/com.yplayer.service`.

### Uninstall

```bash
scripts/uninstall.sh      # or: just uninstall
```

Stops the service and removes the LaunchAgent plist and `~/.local/bin/yplay`. The cache, library DB, `config.toml`, and logs are kept.

## Usage

`yplay --help`:

```
Fast YouTube audio player with local cache

Usage: yplay [OPTIONS] [COMMAND]

Commands:
  serve    Run the background service
  add      Add a YouTube URL to an album and play it
  play     Play a track from the library or an album
  pause    Pause playback
  resume   Resume playback
  toggle   Toggle pause
  stop     Stop playback
  next     Next track
  prev     Previous track
  now      Show the current track
  albums   List albums
  search   Search YouTube
  formats  List audio formats for a URL
  help     Print this message or the help of the given subcommand(s)

Options:
      --socket <SOCKET>  Service socket (default: $YPLAY_SOCKET, else <state dir>/yplay.sock)
  -h, --help             Print help
```

Subcommand options (`yplay <command> --help`):

| Command | Options |
|---------|---------|
| `serve` | `--dir <DIR>` cache directory (default: config file, else `~/Music/yt-audio`) |
| `add <URL>` | `--album <ALBUM>` (default: the last used album, else Inbox); `--no-play`; `--wait` wait for the download and report timings |
| `play <TRACK_ID>` | `--album <ALBUM>` album to play from (default: the whole library) |
| `search <QUERY>` | `--limit <LIMIT>` maximum number of results (default 10) |
| `formats <URL>` | — |

`search` and `formats` run the worker directly and do not need the service; every other command except `serve` talks to the running service.

```bash
yplay add "https://www.youtube.com/watch?v=jNQXAC9IVRw" --wait
# Added Me at the zoo → Inbox
# first audio after <x.x>s
# downloaded in <y.y>s
yplay now          # ▶ Title — Uploader  m:ss / m:ss  [album]  (⏸ paused, ■ stopped)
yplay albums       # name  (n songs)
yplay toggle
```

Only single YouTube videos are accepted; playlist URLs are rejected. Adding a URL that is already cached plays it immediately (`cached — playing` with `--wait`).

Exit codes: `0` success; `1` the service returned an error (its message is printed); `2` the service is not running (`yplay service is not running — start it with: yplay serve`).

## Configuration

`~/Library/Application Support/yplayer/config.toml` (every key optional):

| Key | Meaning |
|-----|---------|
| `cache_dir` | Cache directory (default `~/Music/yt-audio`) |
| `volume` | Initial volume, 0.0–1.0 (the last volume in session state wins) |
| `api_key` | YouTube Data API key for `yplay search` (else `YT_API_KEY`) |
| `worker_python` | Absolute path of the worker's Python (written by `install.sh`) |

Environment variables:

| Variable | Effect |
|----------|--------|
| `YPLAY_SOCKET` | Service socket path (overridden by `--socket`) |
| `YPLAY_MPV_EXTRA_ARGS` | Extra mpv arguments, whitespace-separated, appended by the service (e.g. `--ao=null`) |
| `YPLAY_NO_UPDATE` | When set, the service skips the yt-dlp update check |
| `YPLAY_WORKER_CMD` | Worker command, whitespace-separated, replacing `<worker_python> -m yplayer.worker` (tests use a fake worker) |

Files:

| Path | Contents |
|------|----------|
| `~/Library/Application Support/yplayer/` | `config.toml`, `yplay.sock`, `yplay.mpv.sock`, `worker.log`, yt-dlp update stamp (dir mode 0700, socket 0600) |
| `~/Library/Logs/yplayer/service.log` | Service stdout/stderr under launchd |
| `~/Music/yt-audio/` | Cache and library DB (see below) |

## Cache layout

```
~/Music/yt-audio/
  <Title> [<id8>]/          # per-track folder
    audio.<ext>             # native best-audio stream
    meta.json               # metadata sidecar, written last (completion marker)
  .yplayer.db               # SQLite library: tracks, albums, lyrics
  .yplayer_state.json       # session state (volume, last track)
```

Legacy flat files (`<id>.<ext>` + `<id>.json`) are still recognized, and legacy `albums/*.album.json` files are imported into the DB once.

## Performance check

```bash
scripts/perf-budget.sh idle      # or: just perf idle
scripts/perf-budget.sh playing   # while a track plays
```

Samples the running `yplay serve` and its mpv/Python children with `top` for 20 s and prints average CPU, idle wakeups/s, and max memory per process. Budget: `idle` — service ≤ 0.2 idle wakeups/s, < 10 MB, no mpv or Python worker running; `playing` — service ≤ 0.2 idle wakeups/s, mpv ≤ 60 MB. Exits 0 on PASS, 1 on FAIL.

## Development

```bash
just check    # cargo fmt --check, clippy -D warnings, cargo test, ruff, pytest
just e2e      # ignored tests: real mpv, network, CLI end-to-end
```

## Project structure

```
crates/yplayer/src/
  main.rs            # CLI entry point (clap)
  client.rs          # CLI client for the service socket
  service/           # yplay serve: socket server, core actor, downloads
  protocol.rs        # socket protocol (v1)
  library/           # SQLite library + cache reconciliation
  player/            # mpv IPC, playback engine, queue
  download/          # Python worker bridge and job manager
  lyrics.rs, http.rs, updater.rs, config.rs, types.rs, ytid.rs
yplayer/             # Python package: yt-dlp download worker (worker.py, core.py)
packaging/           # LaunchAgent plist template
scripts/             # install.sh, uninstall.sh, perf-budget.sh
docs/                # design spec and implementation plans
```

## License

MIT
