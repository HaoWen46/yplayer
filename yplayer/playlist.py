from __future__ import annotations

from yt_dlp import YoutubeDL


def is_playlist_url(url: str) -> bool:
    """Heuristic: treat as playlist if it contains list= or /playlist"""
    if not isinstance(url, str):
        return False
    u = url.lower()
    return ("list=" in u) or ("/playlist" in u)


def extract_playlist_entries(url: str) -> list[dict]:
    """
    Return a list of minimal entry dicts for a playlist URL (id/title/webpage_url/duration/uploader).
    Uses yt-dlp in extract_flat mode to avoid downloading.
    """
    ydl_opts = {"quiet": True, "skip_download": True, "extract_flat": True}
    with YoutubeDL(ydl_opts) as ydl:
        info = ydl.extract_info(url, download=False)
    entries = []
    for e in info.get("entries") or []:
        vid = e.get("id") or e.get("url")
        webpage_url = e.get("webpage_url") or (
            f"https://www.youtube.com/watch?v={vid}" if vid else None
        )
        entries.append(
            {
                "id": vid,
                "title": e.get("title"),
                "uploader": e.get("uploader"),
                "webpage_url": webpage_url,
                "duration": e.get("duration"),
            }
        )
    return entries
