from __future__ import annotations

import threading
from typing import List, Dict, Optional

from yt_dlp import YoutubeDL

from .core import resolve_and_maybe_download, find_existing, Options


def is_playlist_url(url: str) -> bool:
    """Heuristic: treat as playlist if it contains list= or /playlist"""
    if not isinstance(url, str):
        return False
    u = url.lower()
    return ("list=" in u) or ("/playlist" in u)


def extract_playlist_entries(url: str) -> List[Dict]:
    """
    Return a list of minimal entry dicts for a playlist URL (id/title/webpage_url/duration/uploader).
    Uses yt-dlp in extract_flat mode to avoid downloading.
    """
    ydl_opts = {"quiet": True, "skip_download": True, "extract_flat": True}
    with YoutubeDL(ydl_opts) as ydl:
        info = ydl.extract_info(url, download=False)
    entries = []
    for e in (info.get("entries") or []):
        vid = e.get("id") or e.get("url")
        webpage_url = e.get("webpage_url") or (f"https://www.youtube.com/watch?v={vid}" if vid else None)
        entries.append({
            "id": vid,
            "title": e.get("title"),
            "uploader": e.get("uploader"),
            "webpage_url": webpage_url,
            "duration": e.get("duration"),
        })
    return entries


class Prefetcher(threading.Thread):
    """
    Background downloader that keeps N tracks ahead cached.
    It is conservative and best-effort: resolve_and_maybe_download will skip already-cached items.
    """
    def __init__(self, entries: List[Dict], opts: Options, prefetch_count: int = 3, api_key: Optional[str] = None):
        super().__init__(daemon=True)
        self.entries = entries
        self.opts = opts
        self.prefetch_count = max(1, int(prefetch_count or 1))
        self.api_key = api_key
        self._stop = threading.Event()
        self._idx = 0

    def stop(self):
        self._stop.set()

    def set_index(self, idx: int):
        self._idx = max(0, idx)

    def run(self):
        while not self._stop.is_set():
            start = self._idx + 1
            end = min(len(self.entries), start + self.prefetch_count)
            for i in range(start, end):
                if self._stop.is_set():
                    break
                entry = self.entries[i]
                # skip if already cached
                try:
                    cached = find_existing(self.opts.cache_dir, entry.get("id"), entry.get("title"))
                except Exception:
                    cached = None
                if cached:
                    entry["path"] = cached
                    continue
                try:
                    resolve_and_maybe_download(entry.get("webpage_url"), self.opts, api_key=self.api_key)
                except Exception:
                    # swallow errors; prefetch is best-effort
                    pass
            # small sleep to yield
            for _ in range(10):
                if self._stop.is_set():
                    break
                import time; time.sleep(0.1)
