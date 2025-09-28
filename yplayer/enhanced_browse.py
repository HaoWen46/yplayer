# yplayer/enhanced_browse.py
import curses
import json
import os
import re
import subprocess
import threading
import time
from typing import List, Dict, Optional, Any

from .enhanced_playback import EnhancedPlayer
from .core import resolve_and_maybe_download, find_existing, Options
from .playlist import Prefetcher
from .albums import AlbumManager

# Browse modes
MODE_LIBRARY = "library"
MODE_ALBUMS = "albums"
MODE_ALBUM_DETAIL = "album_detail"
MODE_PLAYLIST = "playlist"

# Playback control keys
KEY_PLAY_PAUSE = ord(' ')  # Space key for pause/resume
KEY_STOP = ord('s')
KEY_LOOP_TOGGLE = ord('l')
KEY_ALBUMS_VIEW = ord('a')
KEY_BACK = ord('b')
KEY_CREATE_ALBUM = ord('c')

ROW_PAD = 1

# ─── formatting helpers ───────────────────────────────────────────────────────

def _fmt_dur(sec: Optional[int]) -> str:
    if sec is None:
        return "?:??"
    try:
        sec = int(sec)
    except Exception:
        return "?:??"
    m, s = divmod(sec, 60)
    h, m = divmod(m, 60)
    return f"{h:d}:{m:02d}:{s:02d}" if h else f"{m:d}:{s:02d}"


_ISO_DUR_RE = re.compile(
    r'^P(?:(?P<days>\d+)D)?(?:T(?:(?P<hours>\d+)H)?(?:(?P<minutes>\d+)M)?(?:(?P<seconds>\d+)S)?)?$'
)

def _parse_iso8601_duration(s: str) -> Optional[int]:
    """
    Parse ISO-8601 duration like PT3M12S into seconds.
    """
    if not s or not isinstance(s, str):
        return None
    m = _ISO_DUR_RE.match(s)
    if not m:
        return None
    days = int(m.group("days") or 0)
    hours = int(m.group("hours") or 0)
    minutes = int(m.group("minutes") or 0)
    seconds = int(m.group("seconds") or 0)
    return days*86400 + hours*3600 + minutes*60 + seconds


def _probe_duration_ffprobe(path: str) -> Optional[int]:
    """
    Use ffprobe to get duration in seconds.
    """
    if not path or not os.path.exists(path):
        return None
    try:
        out = subprocess.check_output(
            ["ffprobe", "-v", "error", "-show_entries", "format=duration",
             "-of", "default=noprint_wrappers=1:nokey=1", path],
            stderr=subprocess.DEVNULL,
        ).decode("utf-8", "replace").strip()
        if out:
            return int(float(out))
    except Exception:
        return None
    return None


def _read_sidecar(path: str) -> Optional[dict]:
    if not path:
        return None
    base, _ext = os.path.splitext(path)
    sidecar = base + ".json"
    if not os.path.exists(sidecar):
        return None
    try:
        with open(sidecar, "r", encoding="utf-8") as f:
            return json.load(f)
    except Exception:
        return None


def _ensure_item_duration(item: Dict) -> None:
    """
    Ensure item['duration'] is set using (in order):
      1) existing item['duration']
      2) sidecar JSON keys: duration, length_seconds, duration_ms, approx_duration_ms, duration_string (ISO-8601)
      3) ffprobe of cached audio file (if path exists)
    """
    if item.get("duration") is not None:
        return

    # 2) sidecar
    sc = _read_sidecar(item.get("path"))
    if sc:
        keys = (
            "duration",
            "length_seconds",
            "duration_seconds",
            "duration_ms",
            "approx_duration_ms",
            "duration_string",    # maybe ISO-8601
        )
        val = None
        for k in keys:
            if k in sc and sc[k] is not None:
                val = sc[k]
                break
        if isinstance(val, (int, float)):
            item["duration"] = int(val if val > 1000 else val)  # seconds (heuristic if ms handled below)
            return
        if isinstance(val, str):
            # try ISO-8601 or "mm:ss"
            sec = _parse_iso8601_duration(val)
            if sec is None:
                try:
                    parts = [int(p) for p in val.split(":")]
                    if len(parts) == 2:
                        sec = parts[0]*60 + parts[1]
                    elif len(parts) == 3:
                        sec = parts[0]*3600 + parts[1]*60 + parts[2]
                except Exception:
                    sec = None
            if sec is not None:
                item["duration"] = sec
                return
        # if milliseconds
        ms = sc.get("duration_ms") or sc.get("approx_duration_ms")
        if isinstance(ms, (int, float)):
            item["duration"] = int(ms/1000)
            return

    # 3) ffprobe
    p = item.get("path")
    sec = _probe_duration_ffprobe(p) if p else None
    if sec is not None:
        item["duration"] = sec


def _ensure_item_uploader(item: Dict) -> None:
    if item.get("uploader"):
        return
    sc = _read_sidecar(item.get("path"))
    if sc:
        up = sc.get("uploader") or sc.get("artist") or sc.get("channel")
        if isinstance(up, str) and up.strip():
            item["uploader"] = up.strip()


# ─── curses color setup ───────────────────────────────────────────────────────

PAIR_TITLE = 1      # cyan
PAIR_UPLOADER = 2   # yellow
PAIR_DURATION = 3   # magenta
PAIR_CHECK = 4      # green
PAIR_TAG = 5        # blue
PAIR_HINTS = 6      # cyan (dim)
PAIR_HEADER = 7     # white bold
PAIR_SELECTION = 8  # reverse highlight
PAIR_PLAYBACK = 9   # red
PAIR_KEY = 10       # bright yellow for keys
PAIR_DESCRIPTION = 11  # dim white for descriptions

def _init_colors():
    if not curses.has_colors():
        return
    curses.start_color()
    try:
        curses.use_default_colors()
    except Exception:
        pass

    curses.init_pair(PAIR_TITLE, curses.COLOR_CYAN, -1)
    curses.init_pair(PAIR_UPLOADER, curses.COLOR_YELLOW, -1)
    curses.init_pair(PAIR_DURATION, curses.COLOR_MAGENTA, -1)
    curses.init_pair(PAIR_CHECK, curses.COLOR_GREEN, -1)
    curses.init_pair(PAIR_TAG, curses.COLOR_BLUE, -1)
    curses.init_pair(PAIR_HINTS, curses.COLOR_CYAN, -1)
    curses.init_pair(PAIR_HEADER, curses.COLOR_WHITE, -1)
    curses.init_pair(PAIR_PLAYBACK, curses.COLOR_RED, -1)
    curses.init_pair(PAIR_KEY, curses.COLOR_YELLOW, -1)  # Bright yellow for keys
    curses.init_pair(PAIR_DESCRIPTION, curses.COLOR_WHITE, -1)  # White for descriptions

    bg = curses.COLOR_BLUE if hasattr(curses, "COLOR_BLUE") else -1
    curses.init_pair(PAIR_SELECTION, curses.COLOR_WHITE, bg)


# ─── rendering ────────────────────────────────────────────────────────────────

def _render_header(stdscr, title: str, playback_status: str = "", loop_status: str = ""):
    h, w = stdscr.getmaxyx()
    stdscr.attron(curses.A_BOLD | curses.color_pair(PAIR_HEADER))
    header_text = title[: max(0, w - 4)]
    stdscr.addstr(0, 2, header_text)
    stdscr.attroff(curses.A_BOLD | curses.color_pair(PAIR_HEADER))

    status_parts = []
    if playback_status:
        status_parts.append(playback_status)
    if loop_status:
        status_parts.append(loop_status)

    if status_parts:
        status_text = " " + " ".join(status_parts)
        if len(header_text) + len(status_text) < w - 2:
            stdscr.attron(curses.color_pair(PAIR_PLAYBACK) | curses.A_BOLD)
            stdscr.addstr(0, 2 + len(header_text), status_text)
            stdscr.attroff(curses.color_pair(PAIR_PLAYBACK) | curses.A_BOLD)

    stdscr.hline(1, 0, curses.ACS_HLINE, w)


def _render_footer(stdscr, mode: str, player_supports_control: bool, show_album_hint: bool = False):
    h, w = stdscr.getmaxyx()
    stdscr.hline(h - 2, 0, curses.ACS_HLINE, w)

    # Build hint text with color coding
    hints = []

    if mode == MODE_LIBRARY:
        hints.extend([
            ("↑/↓", "move"),
            ("Enter", "play"),
            ("d", "delete"),
            ("r", "rename")
        ])
        if show_album_hint:
            hints.append(("a", "albums"))
    elif mode == MODE_ALBUMS:
        hints.extend([
            ("↑/↓", "select album"),
            ("Enter", "open album"),
            ("b", "back to library")
        ])
    elif mode == MODE_ALBUM_DETAIL:
        hints.extend([
            ("↑/↓", "select track"),
            ("Enter", "play track"),
            ("d", "remove from album"),
            ("b", "back to albums")
        ])

    # Add common controls
    if player_supports_control:
        hints.extend([
            ("Space", "pause/resume"),
            ("s", "stop"),
            ("l", "loop toggle")
        ])
    else:
        hints.extend([
            ("s", "stop"),
            ("l", "loop toggle")
        ])

    hints.append(("q", "quit"))

    # Render with color coding
    x = 2
    for i, (key, desc) in enumerate(hints):
        if x >= w - 4:
            break

        # Render key in bright yellow
        key_text = key
        if x + len(key_text) + 3 < w - 4:  # +3 for " " + desc start
            stdscr.attron(curses.color_pair(PAIR_KEY) | curses.A_BOLD)
            stdscr.addstr(h - 1, x, key_text)
            stdscr.attroff(curses.color_pair(PAIR_KEY) | curses.A_BOLD)
            x += len(key_text)

            # Render description in dim white
            desc_text = f" {desc}"
            if i < len(hints) - 1:
                desc_text += "   "
            if x + len(desc_text) < w - 4:
                stdscr.attron(curses.color_pair(PAIR_DESCRIPTION) | curses.A_DIM)
                stdscr.addstr(h - 1, x, desc_text)
                stdscr.attroff(curses.color_pair(PAIR_DESCRIPTION) | curses.A_DIM)
                x += len(desc_text)


def _render_rows(stdscr, items: List[Dict], sel: int, offset: int, current_playing_idx: Optional[int] = None, mode: str = MODE_LIBRARY):
    h, w = stdscr.getmaxyx()
    top = 2
    bottom = 2
    usable_h = h - top - bottom
    rows_per_item = 1 + ROW_PAD
    if usable_h <= 0:
        return
    max_rows = max(1, usable_h // rows_per_item)
    end = min(len(items), offset + max_rows)

    y = top
    for idx in range(offset, end):
        item = items[idx]

        if mode == MODE_ALBUMS:
            # Album list view
            title = item.get("name", "Unknown Album")
            track_count = item.get("track_count", 0)
            description = item.get("description", "")

            left_parts = [title]
            right_parts = [f"{track_count} tracks"]

        else:
            # Track view (library, album detail, playlist)
            _ensure_item_duration(item)
            _ensure_item_uploader(item)

            title = item.get("title") or os.path.basename(
                item.get("path", "") or item.get("webpage_url", "") or ""
            )
            dur = _fmt_dur(item.get("duration"))
            uploader = item.get("uploader") or ""

            left_parts = []
            if item.get("type") == "playlist":
                left_parts.append("[PL]")
            elif item.get("type") == "album_track":
                left_parts.append("[AL]")
            left_parts.append(title)
            if uploader:
                left_parts.append("—")
                left_parts.append(uploader)

            right_parts = []
            if item.get("path"):
                right_parts.append("✓")
            if item.get("type") == "playlist" and item.get("count"):
                right_parts.append(str(item["count"]))
            right_parts.append(dur)

            # Add playback indicator
            if idx == current_playing_idx:
                left_parts.insert(0, "▶")

        right = " ".join(right_parts)
        max_left = max(0, w - len(right) - 6)

        # Draw selection arrow
        if idx == sel:
            arrow = "→ "
            stdscr.attron(curses.color_pair(PAIR_UPLOADER) | curses.A_BOLD)
            stdscr.addstr(y, 2, arrow)
            stdscr.attroff(curses.color_pair(PAIR_UPLOADER) | curses.A_BOLD)
        else:
            arrow = "  "
            stdscr.addstr(y, 2, arrow)

        # Draw left content
        x = 2 + len(arrow)
        for part in left_parts:
            if part == "▶":
                attr = curses.color_pair(PAIR_PLAYBACK) | curses.A_BOLD
            elif part in ["[PL]", "[AL]"]:
                attr = curses.color_pair(PAIR_TAG) | curses.A_BOLD
            elif part == "—":
                attr = curses.A_DIM
            elif part == uploader:
                attr = curses.color_pair(PAIR_UPLOADER)
            else:
                attr = curses.color_pair(PAIR_TITLE) | curses.A_BOLD
            s = str(part) + " "
            s = s[: max(0, max_left - (x - 2))]
            if s:
                stdscr.attron(attr)
                stdscr.addstr(y, x, s)
                stdscr.attroff(attr)
                x += len(s)

        # Draw right content
        rx = w - len(right) - 2
        cx = rx
        for part in right_parts:
            s = str(part) + " "
            if part == "✓":
                attr = curses.color_pair(PAIR_CHECK) | curses.A_BOLD
            elif "tracks" in str(part) or str(part).isdigit():
                attr = curses.color_pair(PAIR_TAG)
            elif part == dur:
                attr = curses.color_pair(PAIR_DURATION)
            else:
                attr = curses.A_DIM
            stdscr.attron(attr)
            stdscr.addstr(y, cx, s)
            stdscr.attroff(attr)
            cx += len(s)

        y += rows_per_item


# ─── Loop Management ─────────────────────────────────────────────────────────

class LoopManager:
    def __init__(self):
        self.mode = "none"  # "none", "single", "all"

    def toggle(self):
        modes = ["none", "single", "all"]
        current_idx = modes.index(self.mode)
        self.mode = modes[(current_idx + 1) % len(modes)]
        return self.mode

    def get_status_text(self):
        if self.mode == "none":
            return ""
        elif self.mode == "single":
            return "[LOOP: SINGLE]"
        else:
            return "[LOOP: ALL]"


# ─── Browse State Management ─────────────────────────────────────────────────

class BrowseState:
    def __init__(self, library_tracks: List[Dict], player: EnhancedPlayer, opts: Options, api_key: Optional[str] = None):
        self.library_tracks = library_tracks
        self.player = player
        self.opts = opts
        self.api_key = api_key
        self.album_manager = AlbumManager(opts.cache_dir if hasattr(opts, 'cache_dir') else None)

        # Current state
        self.mode = MODE_LIBRARY
        self.current_items = library_tracks.copy()
        self.selection = 0
        self.offset = 0
        self.current_playing_idx = None
        self.current_album_path = None
        self.loop_manager = LoopManager()

        # Prefetcher for playlists
        self.prefetcher = None

        # Track what we're currently playing for loop detection
        self.currently_playing_file = None

    def switch_to_albums(self):
        """Switch to albums view"""
        albums = self.album_manager.list_albums()
        if albums:
            self.mode = MODE_ALBUMS
            self.current_items = albums
            self.selection = 0
            self.offset = 0
            # Don't reset current_playing_idx - music should continue
        else:
            # No albums, stay in library
            pass

    def enter_album(self, album_idx: int):
        """Enter a specific album"""
        if album_idx >= len(self.current_items):
            return

        album = self.current_items[album_idx]
        album_path = album.get("path")
        if not album_path:
            return

        tracks = self.album_manager.get_album_tracks(album_path)
        if tracks:
            self.mode = MODE_ALBUM_DETAIL
            self.current_items = tracks
            self.current_album_path = album_path
            self.selection = 0
            self.offset = 0
            # Don't reset current_playing_idx - music should continue

    def go_back(self):
        """Navigate back in hierarchy"""
        if self.mode == MODE_ALBUM_DETAIL:
            self.switch_to_albums()
        elif self.mode == MODE_ALBUMS:
            self.mode = MODE_LIBRARY
            self.current_items = self.library_tracks.copy()
            # Don't reset selection/playing state
        elif self.mode == MODE_PLAYLIST:
            self.mode = MODE_LIBRARY
            self.current_items = self.library_tracks.copy()
            # Don't reset selection/playing state

    def play_item(self, idx: int):
        """Play the item at given index - ensures only one song plays"""
        if idx >= len(self.current_items):
            return

        item = self.current_items[idx]
        path = item.get("path")

        if not path:
            # Need to download first
            url = item.get("webpage_url")
            if url:
                try:
                    path = resolve_and_maybe_download(url, self.opts, api_key=self.api_key)
                    item["path"] = path
                    _ensure_item_duration(item)
                    _ensure_item_uploader(item)
                except Exception:
                    return

        if path and os.path.exists(path):
            # This will automatically stop any existing playback
            success = self.player.play(path, volume=self.opts.volume)
            if success:
                self.current_playing_idx = idx
                self.currently_playing_file = path
            else:
                self.current_playing_idx = None
                self.currently_playing_file = None

    def check_and_handle_loop(self):
        """Check if current song finished and handle loop logic"""
        # If not playing, check if we should loop
        if not self.player.is_playing() and self.currently_playing_file:
            # Song finished - handle loop
            loop_mode = self.loop_manager.mode
            if loop_mode == "single":
                # Replay the same song
                if self.current_playing_idx is not None:
                    self.play_item(self.current_playing_idx)
            elif loop_mode == "all":
                # Play next song in current context
                if self.current_playing_idx is not None:
                    next_idx = (self.current_playing_idx + 1) % len(self.current_items)
                    self.selection = next_idx
                    # Auto-scroll to keep selection visible
                    h, w = curses.LINES, curses.COLS
                    usable_h = h - 4
                    rows_per_item = 1 + ROW_PAD
                    max_rows = max(1, usable_h // rows_per_item)
                    if next_idx >= self.offset + max_rows:
                        self.offset = next_idx - max_rows + 1
                    elif next_idx < self.offset:
                        self.offset = next_idx
                    self.play_item(next_idx)
            else:
                # No loop - clear playing state
                self.current_playing_idx = None
                self.currently_playing_file = None

    def get_playback_status(self) -> str:
        """Get current playback status string"""
        if not self.player.is_playing():
            return ""
        if self.player.is_paused():
            return "[PAUSED]"
        return "[PLAYING]"

    def get_loop_status(self) -> str:
        """Get current loop status"""
        return self.loop_manager.get_status_text()

    def handle_key(self, ch: int) -> bool:
        """Handle key press, return True if should continue, False if should quit"""
        h, w = curses.LINES, curses.COLS

        if ch in (ord('q'), 27):  # q or Esc
            return False
        elif ch == KEY_BACK:  # b key
            self.go_back()
        elif ch == KEY_ALBUMS_VIEW and self.mode == MODE_LIBRARY:  # a key in library
            self.switch_to_albums()
        elif ch in (curses.KEY_DOWN, ord('j')):
            self.selection = min(len(self.current_items) - 1, self.selection + 1)
            usable_h = h - 4
            rows_per_item = 1 + ROW_PAD
            max_rows = max(1, usable_h // rows_per_item)
            if self.selection >= self.offset + max_rows:
                self.offset = self.selection - max_rows + 1
        elif ch in (curses.KEY_UP, ord('k')):
            self.selection = max(0, self.selection - 1)
            if self.selection < self.offset:
                self.offset = self.selection
        elif ch in (curses.KEY_NPAGE,):
            self.selection = min(len(self.current_items) - 1, self.selection + 10)
        elif ch in (curses.KEY_PPAGE,):
            self.selection = max(0, self.selection - 10)
        elif ch in (ord('d'),):
            if self.mode == MODE_LIBRARY:
                # Delete from library
                item = self.current_items[self.selection]
                p = item.get("path")
                if p and os.path.exists(p):
                    try:
                        os.remove(p)
                    except Exception:
                        pass
                    base, _ = os.path.splitext(p)
                    for ext in (".json",):
                        try:
                            os.remove(base + ext)
                        except Exception:
                            pass
                    item["path"] = None
                    item["duration"] = None
                    if self.selection == self.current_playing_idx:
                        self.player.stop()
                        self.current_playing_idx = None
                        self.currently_playing_file = None
            elif self.mode == MODE_ALBUM_DETAIL:
                # Remove from album only
                item = self.current_items[self.selection]
                track_id = item.get("id")
                if track_id and self.current_album_path:
                    self.album_manager.remove_track_from_album(self.current_album_path, track_id)
                    # Refresh album tracks
                    self.current_items = self.album_manager.get_album_tracks(self.current_album_path)
                    self.selection = min(self.selection, len(self.current_items) - 1)
        elif ch in (ord('r'),) and self.mode == MODE_LIBRARY:
            item = self.current_items[self.selection]
            p = item.get("path")
            if p and os.path.exists(p):
                base = os.path.dirname(p)
                newname = item.get("title") or os.path.basename(p)
                newname = "".join(ch for ch in newname if ch not in '/\\:*?"<>|').strip() or "track"
                newp = os.path.join(base, newname + os.path.splitext(p)[1])
                try:
                    os.rename(p, newp)
                    item["path"] = newp
                except Exception:
                    pass
        elif ch in (10, 13, curses.KEY_ENTER):
            if self.mode == MODE_ALBUMS:
                self.enter_album(self.selection)
            else:
                # Play the selected item (replaces current playback)
                self.play_item(self.selection)
        elif ch == KEY_PLAY_PAUSE:
            if self.player.is_playing():
                self.player.pause_resume()
        elif ch == KEY_STOP:
            self.player.stop()
            self.current_playing_idx = None
            self.currently_playing_file = None
        elif ch == KEY_LOOP_TOGGLE:
            self.loop_manager.toggle()

        return True


def _browse_loop(stdscr, state: BrowseState):
    curses.curs_set(0)
    _init_colors()
    stdscr.timeout(100)  # 100ms refresh for smooth UI without lag

    while True:
        # Check for loop handling
        state.check_and_handle_loop()

        playback_status = state.get_playback_status()
        loop_status = state.get_loop_status()

        stdscr.clear()
        title = {
            MODE_LIBRARY: "Yplayer — Library",
            MODE_ALBUMS: "Yplayer — Albums",
            MODE_ALBUM_DETAIL: "Yplayer — Album Tracks",
            MODE_PLAYLIST: "Yplayer — Playlist"
        }.get(state.mode, "Yplayer")

        _render_header(stdscr, title, playback_status, loop_status)
        _render_rows(stdscr, state.current_items, state.selection, state.offset,
                    state.current_playing_idx, state.mode)
        _render_footer(stdscr, state.mode, state.player.supports_pause(),
                      show_album_hint=(state.mode == MODE_LIBRARY))
        stdscr.refresh()

        try:
            ch = stdscr.getch()
            if ch == -1:
                continue  # Timeout, just refresh
            else:
                if not state.handle_key(ch):
                    break
        except Exception:
            pass


# ─── public entry points ──────────────────────────────────────────────────────

def enhanced_browse_and_play(tracks: List[Dict], prefer_player: Optional[str] = None, volume: Optional[float] = None, cache_dir: str = None):
    if not tracks:
        print("No cached tracks found.")
        return

    for t in tracks:
        _ensure_item_duration(t)
        _ensure_item_uploader(t)

    opts = type('obj', (object,), {
        'volume': volume,
        'cache_dir': cache_dir or os.path.expanduser("~/Music/yt-audio")
    })()

    player = EnhancedPlayer(prefer=prefer_player)
    state = BrowseState(tracks, player, opts)

    try:
        curses.wrapper(_browse_loop, state)
    finally:
        player.stop()


def enhanced_browse_playlist(entries: List[Dict], opts: Options, prefetch_count: int = 3, api_key: Optional[str] = None,
                            prefer_player: Optional[str] = None, volume: Optional[float] = None):
    for e in entries:
        try:
            p = find_existing(opts.cache_dir, e.get("id"), e.get("title"))
        except Exception:
            p = None
        if p:
            e["path"] = p
            _ensure_item_duration(e)
            _ensure_item_uploader(e)

    player = EnhancedPlayer(prefer=prefer_player)
    state = BrowseState([], player, opts, api_key)  # Start with empty library
    state.mode = MODE_PLAYLIST
    state.current_items = entries

    pf = Prefetcher(entries, opts, prefetch_count=prefetch_count, api_key=api_key)
    pf.start()

    try:
        curses.wrapper(_browse_loop, state)
    finally:
        pf.stop()
        player.stop()
