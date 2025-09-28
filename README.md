# Yplayer – CLI YouTube Audio Player

A lightweight CLI tool for caching and playing YouTube audio on your own terms.

* **macOS-first**, but works anywhere with Python + ffmpeg/mpv.
* Uses the **YouTube Data API** for fast metadata (title, uploader, duration).
* Uses **yt-dlp** only for audio download.
* Saves audio in a human-friendly, title-based structure (per-track folders).
* Keeps a **JSON sidecar** with ID + metadata alongside each track.
* Includes a curses **browse menu**: navigate with ↑/↓, play with Enter, delete with `d`, rename with `r`. The UI shows duration, uploader, cached-check, and a yellow `→` marker for the current selection.

---

## Features

* Fast search via YouTube Data API (no descriptions, just title/uploader/duration).
* Download audio-only tracks (`mp3` by default) via `yt-dlp` with optional ffmpeg conversion/embedding.
* Cache downloaded tracks locally and re-use on next play.
* Logical playlist support:

  * Pass a playlist URL and Yplayer will show it as a playlist in Browse.
  * Play items immediately while a background prefetcher downloads the next N tracks.
  * Prefetch is configurable (default 3) and **never** downloads the whole playlist at once.
* Browse local library in a curses-based menu with colorized, dark-theme-friendly layout.
* Delete or rename tracks directly in the browse menu — deletes remove both audio and metadata cleanly.
* Plays audio via your system media player (mpv/ffplay/afplay).
* Backwards-compatible with the legacy flat cache layout (existing files and sidecars still usable).

---

## Storage / Cache layout

Yplayer now uses a **per-track folder layout** for new downloads. This keeps audio + metadata bundled and avoids leftover JSON files when deleting or renaming tracks.

New downloads are stored like:

```
~/.cache/yplayer/<Sanitized Title> [<id8>]/
    audio.<ext>       # the audio file (mp3/m4a/...)
    meta.json         # sidecar with id/title/uploader/duration/url
```

For compatibility the tool also writes/reads the legacy sidecar at:

```
~/.cache/yplayer/<video_id>.json
```

and will still discover and play older, flat-layout files like:

```
~/.cache/yplayer/<Title>.mp3
~/.cache/yplayer/<Title>.json
```

**Why per-track folders?**

* Bundles audio + metadata (and future extras like cover art or lyrics) together.
* Safe and simple deletion (`rmtree` the folder) — no orphaned JSON left behind.
* Avoids filename collisions and keeps the cache tidy.

---

## Browse UI details

The curses browse menu shows:

* Title (cyan, bold)
* Uploader / channel (yellow)
* Duration (magenta) — read from sidecar when present, otherwise computed with `ffprobe` if available
* Cached indicator (green checkmark `✓`)
* `[PL]` playlist tag (blue) for logical playlists
* A **bold yellow `→`** at the left of the current selection (no background highlight)
* Footer hints: `↑/↓ move   Enter play   d delete   r rename   q quit`

The UI will populate duration and uploader fields from the per-track `meta.json` or the legacy `<id>.json`. If duration is missing and `ffprobe` is installed (`ffmpeg` package), Yplayer will probe the file so the UI shows an accurate length.

---

## Playlist behavior

* When you give a playlist URL (or open one from Browse), Yplayer creates a **logical playlist** manifest and shows the track list to you — titles, uploader, durations.
* Playback of any item works immediately:

  * If the track is cached → play instantly.
  * If not cached → download that one track on-demand, then play.
* A background prefetch worker keeps a sliding window of the next `N` tracks downloaded in the background (configurable via `--prefetch`).
* Prefetch is **best-effort**: existing cached tracks are skipped, failures are ignored, and prefetching runs in a daemon thread so it never blocks playback.

---

## Installation

```bash
git clone https://github.com/you/yplayer.git
cd yplayer
python3 -m venv .venv
source .venv/bin/activate
pip install -e .
```

### Requirements

* `yt-dlp`
* `ffmpeg` (recommended — used for conversion, metadata embedding, and `ffprobe` duration probing)
* `python-dotenv` (optional — for `.env` support)
* `mpv` / `ffplay` / other system player (for playback)

If `ffmpeg` is missing, Yplayer will still download native audio formats but some features (conversion, embedding metadata, `ffprobe` fallback) will be limited.

---

## API Key Setup

Yplayer uses the **YouTube Data API** to return fast, accurate metadata (title/uploader/duration) for searches and playlist manifests.

1. Get an API key from [Google Cloud Console](https://console.cloud.google.com/).
2. Create a `.env` file in the project root:

```
YT_API_KEY=your_api_key_here
```

3. Yplayer will auto-load `.env` on startup. Note: some features (like fetching durations for search results and playlist manifests) require an API key.

---

## Usage

### Search (top 10 results)

```bash
yplay "zutomayo"
```

Displays top results with title, uploader, duration, and URL.

### Play by URL (single video)

```bash
yplay "https://youtu.be/abc123xyz"
```

* If cached → play instantly
* If not cached → download into a per-track folder and play after download

### Play a playlist (logical playlist + prefetch)

```bash
yplay --prefetch 5 "https://www.youtube.com/playlist?list=PLxyz"
```

* Opens the playlist as a logical list (titles visible in Browse).
* Prefetch window size is controlled by `--prefetch` (default 3).
* Only a few tracks are downloaded ahead — the entire playlist is **not** downloaded at once.

### Browse local library

```bash
yplay --browse
```

Keys:

* ↑/↓ move
* Enter → play
* `d` → delete selected (removes audio + metadata cleanly)
* `r` → rename selected (renames folder for per-track layout; renames file for legacy layout)
* `q` → quit

---

## Project Layout

```
yplayer/
├── yplayer/
│   ├── __init__.py
│   ├── cli.py
│   ├── core.py
│   ├── playlist.py        # logical playlist handling + prefetcher
│   ├── playback.py
│   ├── browse.py
│   ├── utils.py
│   └── config.py
├── tests/
│   ├── __init__.py
│   └── test_core.py
├── scripts/
│   └── dev.sh
├── .gitignore
├── README.md
├── requirements.txt
├── pyproject.toml
└── LICENSE
```

---

## Notes & Implementation details

* New downloads use a **per-track folder** (`<Sanitized Title> [id8]`) containing `audio.<ext>` and `meta.json`. This keeps related files together (audio, metadata, thumbnails).
* For compatibility the tool still writes legacy sidecars at `<cache>/<video_id>.json`.
* The browse UI reads metadata from the per-track `meta.json` (preferred) or the legacy `<id>.json`. If duration is missing and `ffprobe` is available, Yplayer will probe the audio file so the UI shows a duration.
* Deleting a track in the UI removes the whole folder for per-track layout, or the audio file + `<basename>.json` for legacy layout — no JSON left behind.
* Prefetching runs in a background daemon thread and is intentionally best-effort and non-blocking.
* If `yt-dlp` breaks while downloading, the app will attempt to self-update the `yt-dlp` CLI and retry (best-effort).
* You can integrate external tools/scripts by scanning the cache directory — per-track folders make it easy to find `meta.json` files.
