# Yplayer – Terminal-Based YouTube Music Player

A modern, feature-rich terminal music player for your YouTube audio library with album support, smooth playback controls, and persistent browsing.

* **macOS-first**, but works anywhere with Python + mpv/ffmpeg.
* Uses **yt-dlp** for downloads and **mpv** for smooth playback with pause/resume.
* Organizes music into **albums** while sharing cached files across collections.
* Full **persistent UI**: browse your library while music plays in the background.
* Enhanced **curses interface** with color-coded key hints and multi-level navigation.

---

## 🎵 Key Features

### Core Playback
* **Smooth pause/resume** with Space key (requires mpv)
* **Stop playback** with `s` key
* **Single song enforcement** – only one track plays at a time
* **Loop modes**: toggle between None → Single → All with `l` key
* **Persistent playback** – music continues while you navigate between views

### Library Management
* **Album support**: create logical album groupings without duplicating files
* **Multi-level browsing**: Library → Albums → Album Tracks
* **Smart caching**: per-track folders with bundled metadata
* **Format agnostic**: plays any YouTube audio format (webm/opus, m4a, mp3, etc.)

### Enhanced Browse UI
* **Color-coded key hints**: keys in bright yellow, descriptions in dim white
* **Playback indicators**: ▶ shows currently playing track
* **Status display**: [PLAYING]/[PAUSED] and [LOOP: SINGLE]/[LOOP: ALL]
* **Keyboard navigation**: intuitive controls that work while music plays

### Playlist & Search
* **Logical playlists**: browse and play without downloading entire playlists
* **Background prefetching**: smart downloading of upcoming tracks
* **Fast metadata**: YouTube Data API for accurate titles, durations, and uploaders

---

## 🗂️ Storage Layout

Yplayer uses a **per-track folder layout** for new downloads:

```
~/.cache/yplayer/<Sanitized Title> [<id8>]/
    audio.<ext>       # audio file (webm/m4a/mp3/...)
    meta.json         # metadata: id, title, uploader, duration, URL
```

**Albums** are stored separately as JSON definitions:

```
~/.cache/yplayer/albums/<Album Name>.album.json
    {
      "name": "Album Name",
      "description": "Album description",
      "tracks": [
        {"id": "video_id", "title": "Track Title", "order": 1}
      ]
    }
```

**Backward compatibility**: Still supports legacy flat layout and discovers existing files.

---

## 🎮 Browse UI Controls

### Navigation Modes
* **Library View**: All cached tracks in your collection
* **Albums View**: List of your created albums (`a` key from Library)
* **Album Detail**: Tracks within a specific album

### Universal Controls
* `↑/↓` or `j/k` – Navigate selection
* `Enter` – Play selected track (replaces current playback)
* `Space` – Pause/resume (mpv only)
* `s` – Stop playback
* `l` – Toggle loop mode (None → Single → All)
* `q` or `Esc` – Quit

### Context-Specific Controls
* **Library View**:
  * `d` – Delete track (removes audio + metadata)
  * `r` – Rename track
  * `a` – Switch to Albums view
* **Albums View**:
  * `b` – Back to Library
* **Album Detail View**:
  * `d` – Remove track from album (keeps file in library)
  * `b` – Back to Albums list

### Visual Indicators
* **▶** – Currently playing track
* **✓** – Cached/downloaded track
* **[AL]** – Album track
* **[PL]** – Playlist track
* **Status bar**: Shows playback state and loop mode

---

## 📦 Installation

```bash
git clone https://github.com/HaoWen46/yplayer.git
cd yplayer
python3 -m venv .venv
source .venv/bin/activate
pip install -e .
```

### Requirements

* **mpv** (recommended) – for smooth pause/resume and universal format support
  * macOS: `brew install mpv`
  * Ubuntu/Debian: `sudo apt install mpv`
* **yt-dlp** – for YouTube downloads
* **ffmpeg** (optional) – for format conversion and duration probing
* **python-dotenv** (optional) – for `.env` file support

> **Note**: While afplay/ffplay work for basic playback, **mpv is strongly recommended** for the full music player experience with pause/resume functionality.

---

## 🔑 API Key Setup

Yplayer uses the **YouTube Data API** for fast, accurate metadata.

1. Get an API key from [Google Cloud Console](https://console.cloud.google.com/)
2. Create a `.env` file in the project root:
   ```env
   YT_API_KEY=your_api_key_here
   ```
3. Yplayer auto-loads `.env` on startup

---

## 🚀 Usage

### Play Immediately
```bash
# Search and play top result
yplay "artist name"

# Play specific video
yplay "https://youtu.be/abc123xyz"
```

### Browse Your Library
```bash
# Open persistent music player interface
yplay --browse
```

### Play Playlists
```bash
# Play with background prefetching (default: 3 tracks ahead)
yplay --prefetch 5 "https://www.youtube.com/playlist?list=PLxyz"
```

### Album Management
Albums are managed through the browse interface:
1. Start with `yplay --browse`
2. Press `a` to switch to Albums view
3. Navigate and create/manage albums (future enhancement)

---

## 🏗️ Project Structure

```
yplayer/
├── yplayer/
│   ├── __init__.py
│   ├── cli.py
│   ├── core.py
│   ├── enhanced_playback.py    # mpv IPC + pause/resume
│   ├── mpv_player.py          # mpv-specific implementation
│   ├── enhanced_browse.py     # Multi-level browse UI
│   ├── albums.py              # Album management system
│   ├── playlist.py
│   ├── utils.py
│   └── config.py
├── tests/
├── scripts/
├── README.md
├── requirements.txt
└── pyproject.toml
```

---

## 💡 Design Philosophy

* **Terminal-first**: Optimized for keyboard navigation and terminal workflows
* **Non-destructive**: Albums reference existing files; no duplication
* **Persistent**: UI stays open during playback for continuous interaction
* **Professional audio**: mpv provides smooth, glitch-free playback experience
* **Future-ready**: Architecture supports additional features like album creation, cover art, and more

Yplayer transforms your terminal into a full-featured music player that respects your existing YouTube audio library while adding modern music player capabilities.
