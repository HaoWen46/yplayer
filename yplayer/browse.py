import curses
import json
import os
import re
import subprocess
from typing import List, Dict, Optional

from .playback import Player
from .core import resolve_and_maybe_download, find_existing, Options
from .playlist import Prefetcher

ROW_PAD = 1  # spacing between rows


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

PAIR_TITLE     = 1  # cyan
PAIR_UPLOADER  = 2  # yellow
PAIR_DURATION  = 3  # magenta
PAIR_CHECK     = 4  # green
PAIR_TAG       = 5  # blue
PAIR_HINTS     = 6  # cyan (dim via attr)
PAIR_HEADER    = 7  # white bold
PAIR_SELECTION = 8  # reverse-esque highlight

def _init_colors():
    if not curses.has_colors():
        return
    curses.start_color()
    try:
        curses.use_default_colors()
    except Exception:
        pass
    curses.init_pair(PAIR_TITLE,     curses.COLOR_CYAN,   -1)
    curses.init_pair(PAIR_UPLOADER,  curses.COLOR_YELLOW, -1)
    curses.init_pair(PAIR_DURATION,  curses.COLOR_MAGENTA,-1)
    curses.init_pair(PAIR_CHECK,     curses.COLOR_GREEN,  -1)
    curses.init_pair(PAIR_TAG,       curses.COLOR_BLUE,   -1)
    curses.init_pair(PAIR_HINTS,     curses.COLOR_CYAN,   -1)
    curses.init_pair(PAIR_HEADER,    curses.COLOR_WHITE,  -1)
    bg = curses.COLOR_BLUE if hasattr(curses, "COLOR_BLUE") else -1
    curses.init_pair(PAIR_SELECTION, curses.COLOR_WHITE, bg)


# ─── rendering ────────────────────────────────────────────────────────────────

def _render_header(stdscr, title: str):
    h, w = stdscr.getmaxyx()
    stdscr.attron(curses.A_BOLD | curses.color_pair(PAIR_HEADER))
    stdscr.addstr(0, 2, title[: max(0, w - 4)])
    stdscr.attroff(curses.A_BOLD | curses.color_pair(PAIR_HEADER))
    stdscr.hline(1, 0, curses.ACS_HLINE, w)


def _render_footer(stdscr):
    h, w = stdscr.getmaxyx()
    stdscr.hline(h - 2, 0, curses.ACS_HLINE, w)
    hint = "↑/↓ move   Enter play   d delete   r rename   q quit"
    stdscr.attron(curses.color_pair(PAIR_HINTS) | curses.A_DIM)
    stdscr.addstr(h - 1, 2, hint[: max(0, w - 4)])
    stdscr.attroff(curses.color_pair(PAIR_HINTS) | curses.A_DIM)


def _render_rows(stdscr, items: List[Dict], sel: int, offset: int):
    h, w = stdscr.getmaxyx()
    top = 2  # header + line
    bottom = 2  # footer + line
    usable_h = h - top - bottom
    rows_per_item = 1 + ROW_PAD
    if usable_h <= 0:
        return
    max_rows = max(1, usable_h // rows_per_item)
    end = min(len(items), offset + max_rows)

    y = top
    for idx in range(offset, end):
        item = items[idx]
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

        right = " ".join(right_parts)
        max_left = max(0, w - len(right) - 6)

        # Draw arrow for selected row (bold yellow), spaces otherwise
        if idx == sel:
            arrow = "→ "
            stdscr.attron(curses.color_pair(PAIR_UPLOADER) | curses.A_BOLD)
            stdscr.addstr(y, 2, arrow)
            stdscr.attroff(curses.color_pair(PAIR_UPLOADER) | curses.A_BOLD)
        else:
            arrow = "  "
            stdscr.addstr(y, 2, arrow)

        # Draw left (title + uploader)
        x = 2 + len(arrow)
        for part in left_parts:
            if part == "[PL]":
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

        # Draw right (checkmark, count, duration)
        rx = w - len(right) - 2
        cx = rx
        for part in right_parts:
            s = str(part) + " "
            if part == "✓":
                attr = curses.color_pair(PAIR_CHECK) | curses.A_BOLD
            elif part == dur:
                attr = curses.color_pair(PAIR_DURATION)
            else:
                attr = curses.A_DIM
            stdscr.attron(attr)
            stdscr.addstr(y, cx, s)
            stdscr.attroff(attr)
            cx += len(s)

        y += rows_per_item


# ─── interaction ─────────────────────────────────────────────────────────────

def _choose_file(stdscr, items: List[Dict]):
    curses.curs_set(0)
    _init_colors()

    sel = 0
    offset = 0
    while True:
        stdscr.clear()
        _render_header(stdscr, "Yplayer — Browse")
        _render_rows(stdscr, items, sel, offset)
        _render_footer(stdscr)
        stdscr.refresh()

        ch = stdscr.getch()
        if ch in (ord('q'), 27):  # q or Esc
            return None
        elif ch in (curses.KEY_DOWN, ord('j')):
            sel = min(len(items) - 1, sel + 1)
            h, w = stdscr.getmaxyx()
            top = 2
            bottom = 2
            usable_h = h - top - bottom
            rows_per_item = 1 + ROW_PAD
            max_rows = max(1, usable_h // rows_per_item)
            if sel >= offset + max_rows:
                offset = sel - max_rows + 1
        elif ch in (curses.KEY_UP, ord('k')):
            sel = max(0, sel - 1)
            if sel < offset:
                offset = sel
        elif ch in (curses.KEY_NPAGE,):
            sel = min(len(items) - 1, sel + 10)
        elif ch in (curses.KEY_PPAGE,):
            sel = max(0, sel - 10)
        elif ch in (ord('d'),):
            item = items[sel]
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
        elif ch in (ord('r'),):
            item = items[sel]
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
            return items[sel]


# ─── public entry points ──────────────────────────────────────────────────────

def browse_and_play(tracks: List[Dict], prefer_player: Optional[str] = None, volume: Optional[float] = None):
    if not tracks:
        print("No cached tracks found.")
        return

    for t in tracks:
        _ensure_item_duration(t)
        _ensure_item_uploader(t)

    def _ui(stdscr):
        return _choose_file(stdscr, tracks)

    chosen = curses.wrapper(_ui)
    if not chosen:
        return

    player = Player(prefer=prefer_player)
    player.play(chosen["path"], volume=volume)


def browse_playlist(entries: List[Dict], opts: Options, prefetch_count: int = 3, api_key: Optional[str] = None,
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

    def _ui(stdscr):
        return _choose_file(stdscr, entries)

    pf = Prefetcher(entries, opts, prefetch_count=prefetch_count, api_key=api_key)
    pf.start()

    chosen = curses.wrapper(_ui)
    pf.stop()

    if not chosen:
        return

    path = chosen.get("path")
    if not path:
        url = chosen.get("webpage_url")
        path = resolve_and_maybe_download(url, opts, api_key=api_key)
        chosen["path"] = path
        _ensure_item_duration(chosen)
        _ensure_item_uploader(chosen)

    Player(prefer=prefer_player).play(path, volume=volume)
