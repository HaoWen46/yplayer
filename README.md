# Yplayer

A YouTube audio player for macOS that plays from a local cache. Drag a YouTube link from Safari or Chrome onto the floating orb: the song goes into an album and starts playing within a few seconds, and every later play is offline. A menu-bar app shows the library and controls playback; the `yplay` CLI controls the same background service from a terminal.

## Architecture

```
 Yplayer.app (menu bar + orb) ──┐
 yplay CLI (add/play/pause/…) ──┤ Unix socket, newline-delimited JSON
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
| **macOS 26** | Yes | The menu-bar app's Liquid Glass UI |
| **Xcode Command Line Tools** | Yes | Swift 6.2, builds the menu-bar app (`xcode-select --install`) |
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
5. Stops the service if it is loaded, then backs up the library DB once: `<cache_dir>/.yplayer.db` to `<cache_dir>/.yplayer.db.pre-service.bak` (owner-only) if the DB exists and no backup exists yet (an existing backup is never overwritten).
6. Starts the service with `launchctl bootstrap gui/$UID` and prints its status.
7. Builds the menu-bar app and installs it with its own LaunchAgent (see [Menu-bar app](#menu-bar-app)).

Check the service with `launchctl print gui/$UID/com.yplayer.service`.

### Uninstall

```bash
scripts/uninstall.sh      # or: just uninstall
```

Stops the service and the app and removes their LaunchAgent plists, `~/.local/bin/yplay`, and `~/Applications/Yplayer.app`. The cache, library DB, `config.toml`, and logs are kept.

## Menu-bar app

`Yplayer.app` lives in the menu bar only (no Dock icon). It is a client of the service: it needs `yplay serve` running (the `com.yplayer.service` LaunchAgent) and never plays audio itself. While it cannot reach the service it shows "Can't reach the yplay service — retrying in Ns" with a Retry Now button.

- Status item: `music.note` when not playing, `waveform` while playing; click to open or close the popover.
- Now-playing card: artwork, title, uploader, scrubber with elapsed/remaining time, previous · play/pause · next, loop mode (none → all → single → shuffle), volume, lyrics toggle (synced lyrics replace the library while on), and a ⋯ menu (Remove from Album…, Delete from Library…, Show in Finder, Copy YouTube Link).
- Library tabs: Albums · Songs · Search. Albums: + creates an album, double-click a name to rename it, Delete Album… keeps its songs. An album's songs can be reordered by dragging. Downloading rows show progress; failed rows show a warning icon and a Retry menu item.
- Track rows: double-click or Return plays; right-click for Play Next, Add to Album ▸, Remove from Album… (album view), Delete from Library…, Show in Finder, Copy YouTube Link, Retry (failed only), Rename…; trackpad swipe-left removes from the album (album view) or deletes from the library (Songs).
- Deleting from the library moves the audio file to the Trash; every removal and deletion asks for confirmation first.
- Media keys and Control Center's Now Playing show the current song and control the service.

Keyboard shortcuts (popover open):

| Key | Action |
|-----|--------|
| Space | Play/pause (not while typing in a text field) |
| ⌘F | Search the library |
| Return | Play the selected row |
| ⌫ | Remove from album (album view) or delete from library (Songs), after confirmation |
| ⌘⌫ | Delete from library, after confirmation |
| Esc | Close the confirmation, else the popover |

Install and uninstall: `scripts/install.sh` also builds the app with `scripts/build-app.sh`, installs it to `~/Applications/Yplayer.app` (replacing any previous copy), writes `~/Library/LaunchAgents/com.yplayer.app.plist` (`RunAtLoad`; relaunched only after an abnormal exit; GUI login sessions only; logs to `~/Library/Logs/yplayer/app.log`), and restarts it with `launchctl bootstrap gui/$UID`. `scripts/uninstall.sh` also stops the app and removes its LaunchAgent plist and `~/Applications/Yplayer.app`. `just app` only builds `build/Yplayer.app`.

## Drop orb

Start dragging a YouTube link and a glass orb appears at the right edge of the screen under the cursor; it hides again when the drag ends. It needs no permissions.

- Drag over the orb and it fans out: the center is the album you used last (Inbox when there are none), around it up to 5 recent albums and **+ New…**.
- Drop on an album and the song is added there and starts playing right away, even while it is still downloading.
- Drop on **+ New…** to type a name for a new album (Return creates it, Esc cancels).
- An **Undo** toast stays beside the orb for 6 s: Undo takes the song back out of the album and, if it was a new download, moves it to the Trash.
- A link that is not a single YouTube video (a playlist, another site) makes the orb shake with a short message; nothing is added.
- Drag sources: Safari's address bar and page links; Chrome's site icon (left of the address) and page links. Chrome tabs cannot be dragged out of Chrome, so they do not work.

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
| `~/Library/Logs/yplayer/app.log` | Menu-bar app stdout/stderr under launchd |
| `~/Music/yt-audio/` | Cache and library DB (see below) |

## Cache layout

```
~/Music/yt-audio/
  <Title> [<id8>]/          # per-track folder
    audio.<ext>             # native best-audio stream
    cover.jpg               # artwork (absent when YouTube has none)
    meta.json               # metadata sidecar, written last (completion marker)
  .yplayer.db               # SQLite library: tracks, albums, lyrics
  .yplayer_state.json       # session state (volume, last track)
```

Legacy flat files (`<id>.<ext>` + `<id>.json`) are still recognized, and legacy `albums/*.album.json` files are imported into the DB once.

## Performance check

```bash
scripts/perf-budget.sh idle      # or: just perf idle
scripts/perf-budget.sh playing   # while a track plays
scripts/perf-budget.sh app-closed   # menu-bar app running, popover closed
scripts/perf-budget.sh app-open     # popover open while a track plays
```

Samples the running `yplay serve` and its mpv/Python children (or, for the `app-*` states, the `Yplayer` process) with `top` for 20 s and prints average CPU, idle wakeups/s, and max memory per process. Budget: `idle` — service ≤ 0.2 idle wakeups/s, < 10 MB, no mpv or Python worker running; `playing` — service ≤ 0.2 idle wakeups/s, mpv ≤ 60 MB; `app-closed` — Yplayer ≤ 0.2 idle wakeups/s, < 45 MB; `app-open` — Yplayer ≤ 1.5 idle wakeups/s. Exits 0 on PASS, 1 on FAIL, 2 when the process is not running or more than one is.

## Development

```bash
just check    # what CI runs: Rust (fmt, clippy -D warnings, tests), Python (ruff, pytest), Swift (format lint, build, tests)
just fix      # auto-format Rust, Python and Swift
just e2e      # ignored tests: real mpv, network, CLI end-to-end
```

With only the Command Line Tools (no Xcode), run the Swift tests through `scripts/swift-test.sh`: it passes the Swift Testing plugin path that the tools do not find on their own. Tests never touch the real cache, the installed service or the running app.

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
apps/macos/          # SwiftUI menu-bar app + drop orb (Swift package)
  Sources/YplayerKit/  # UI-free: socket client, protocol, library store, orb logic (tested)
  Sources/Yplayer/     # the app: popover views, orb panels, Now Playing, artwork
yplayer/             # Python package: yt-dlp download worker (worker.py, core.py)
tests/               # Python worker tests
packaging/           # LaunchAgent plist templates (service, app)
scripts/             # install, uninstall, app build, Swift tests, performance check
docs/specs/          # design spec with verification results
docs/plans/          # implementation plans, one per sub-project
```

## License

MIT
