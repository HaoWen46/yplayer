# Yplayer

A fast, terminal-native YouTube audio player built with Rust and Ratatui. Downloads, caches, and plays YouTube audio with a flicker-free TUI, real-time progress tracking, and fuzzy search.

## Architecture

```
Rust binary (yplay)          Python worker (yt-dlp)
┌──────────────────────┐     ┌────────────────────┐
│ Ratatui TUI          │     │ download_audio()   │
│ SQLite cache index   │────>│ search_results()   │
│ mpv IPC (persistent) │     │ playlist_entries() │
│ tokio async runtime  │     │ video_info()       │
└──────────────────────┘     └────────────────────┘
        JSON over stdin/stdout
```

**Rust** handles everything user-facing: the TUI, cache queries, playback control, and concurrency. **Python** stays as a headless download worker because yt-dlp is a Python library that updates constantly to keep pace with YouTube's API changes. yt-dlp auto-updates via pip on each worker start.

## Installation

```bash
git clone https://github.com/HaoWen46/yplayer.git
cd yplayer

# Python dependencies (yt-dlp worker)
python3 -m venv .venv
source .venv/bin/activate
pip install -e .

# Rust binary
cargo build --release
cp target/release/yplay ~/.local/bin/  # or anywhere in your PATH
```

### System dependencies

| Tool | Required | Purpose |
|------|----------|---------|
| **mpv** | Yes | Audio playback with pause/resume, seek, volume |
| **ffmpeg** | Optional | Format conversion and metadata embedding |
| **Python 3.9+** | Yes | Runs the yt-dlp download worker |

```bash
# macOS
brew install mpv ffmpeg

# Ubuntu/Debian
sudo apt install mpv ffmpeg
```

### YouTube Data API key (optional)

Yplayer can use the YouTube Data API for faster search. Without it, search falls back to yt-dlp's built-in search.

1. Get a key from [Google Cloud Console](https://console.cloud.google.com/)
2. Set it in your environment or a `.env` file in the project root:
   ```
   YT_API_KEY=your_key_here
   ```

## Usage

```bash
# Browse your cached library (TUI)
yplay

# Search YouTube and download
yplay "zutomayo ham"

# Play a specific video
yplay "https://youtu.be/abc123xyz"

# Download without playing
yplay --download-only "https://youtu.be/abc123xyz"
```

### CLI flags

| Flag | Default | Description |
|------|---------|-------------|
| `--dir <path>` | `~/Music/yt-audio` | Cache directory |
| `--format <fmt>` | `mp3` | Output format (mp3, m4a, opus, flac, wav) |
| `--native` | | Skip format conversion |
| `--no-meta` | | Skip metadata embedding |
| `--volume <0.0-1.0>` | | Initial volume |
| `--download-only` | | Download to cache, don't play |
| `--list-formats` | | Show available audio formats for a URL |
| `--yt-api-key <key>` | `$YT_API_KEY` | YouTube Data API key |

## TUI Controls

### Navigation

| Key | Action |
|-----|--------|
| `j` / `k` / arrow keys | Move selection |
| `PgUp` / `PgDn` | Jump 10 items |
| `Enter` | Play selected track |
| `a` | Switch to Albums view |
| `b` | Go back |
| `q` / `Esc` | Quit |

### Playback

| Key | Action |
|-----|--------|
| `Space` | Pause / resume |
| `s` | Stop |
| left / right arrows | Seek -5s / +5s |
| `+` / `-` | Volume up / down |
| `n` / `p` | Next / previous track |
| `l` | Cycle loop mode (None -> Single -> All -> Shuffle) |

### Library

| Key | Action |
|-----|--------|
| `/` | Fuzzy search (title + uploader) |
| `S` | Cycle sort (A-Z / Recently Played / Recently Added / By Artist) |
| `D` | Download a new URL without leaving the TUI |
| `r` | Rename selected track |
| `d` | Delete track (press twice to confirm) |

### Visual indicators

| Symbol | Meaning |
|--------|---------|
| playing triangle | Currently playing |
| right arrow | Selected |
| checkmark | Cached / downloaded |
| `[PLAYING]` / `[PAUSED]` | Playback state |
| `[LOOP: SINGLE]` / `[LOOP: ALL]` / `[SHUFFLE]` | Loop mode |
| `[A-Z]` / `[Recently Played]` / etc. | Current sort mode |

## Cache Layout

```
~/Music/yt-audio/
  <Title> [<id8>]/        # per-track folder
    audio.mp3             # audio file
    meta.json             # metadata sidecar
  albums/
    <Name>.album.json     # album definitions (references, not copies)
  .yplayer.db             # SQLite index (auto-generated)
```

The SQLite index is built on first run by scanning the cache directory. Subsequent launches load from the index instantly. New files downloaded via CLI are picked up automatically on the next TUI launch. Legacy flat-layout files (`<id>.mp3` + `<id>.json`) are also supported.

## Project Structure

```
yplayer/
  Cargo.toml                        # Rust workspace
  crates/yplayer-tui/src/
    main.rs                         # CLI entry point (clap)
    app.rs                          # App state machine + event loop
    config.rs                       # Configuration
    types.rs                        # Track, Album, LoopMode, SortMode, ViewMode
    events.rs                       # Keyboard to Action mapping
    ui/                             # Ratatui views
      library.rs, albums.rs, search.rs
      player_bar.rs, input_overlay.rs, theme.rs
    player/mpv.rs                   # Persistent async mpv IPC
    cache/index.rs, scanner.rs      # SQLite index + incremental scanner
    download/bridge.rs, prefetch.rs # Python worker bridge
  yplayer/                          # Python package (download worker)
    worker.py                       # JSON-stdio protocol handler
    core.py                         # yt-dlp download + YouTube API
    cli.py                          # Legacy Python CLI (yplay-py entrypoint)
    playlist.py, albums.py
    config.py, utils.py
  pyproject.toml
```

## Design

- **Efficiency first.** Rust for everything user-facing; Python only where yt-dlp requires it.
- **Flicker-free TUI.** Ratatui differential rendering at 20fps.
- **Instant library.** SQLite index replaces filesystem scans; incremental updates on each launch.
- **Persistent mpv connection.** Single async Unix socket, not open-close per command.
- **Non-destructive albums.** Albums are references to cached tracks, not copies.
- **Auto-updating yt-dlp.** Checks PyPI on worker start, updates via pip if outdated.

## License

MIT
