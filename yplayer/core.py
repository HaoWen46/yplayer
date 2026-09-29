"""Downloads, search and format inspection for the yplayer worker (see worker.py).

A download lands in <cache>/<Title> [<id8>]/ as audio.<ext>, cover.jpg and
meta.json; meta.json is written last and marks the download complete.
Search uses the YouTube Data API; downloads and format listing use yt-dlp.
"""

import json
import os
import stat
import time
import urllib.parse
import urllib.request

from yt_dlp import YoutubeDL
from yt_dlp.utils import DownloadCancelled


class YplayerError(Exception):
    """An expected failure whose message is safe to show to the user.

    The worker returns ``str(YplayerError)`` in the JSON ``error`` field.
    """


# ----------- YouTube Data API (search) ----------

API_BASE = "https://www.googleapis.com/youtube/v3"

def _require_api_key(api_key: str | None) -> str:
    key = api_key or os.environ.get("YT_API_KEY")
    if not key:
        raise YplayerError(
            "YouTube Data API key missing. Set $YT_API_KEY or api_key in config.toml."
        )
    return key

def _http_get_json(url: str, timeout: int = 10) -> dict:
    req = urllib.request.Request(url, headers={"Accept": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))

def _parse_iso8601_duration(s: str | None) -> int | None:
    """Convert ISO-8601 duration (PT#H#M#S) to seconds."""
    if not s or not s.startswith("PT"):
        return None
    h = m = sec = 0
    num = ""
    for ch in s[2:]:
        if ch.isdigit():
            num += ch
        else:
            if not num:
                continue
            val = int(num)
            if ch == "H":
                h = val
            elif ch == "M":
                m = val
            elif ch == "S":
                sec = val
            num = ""
    return h * 3600 + m * 60 + sec

def yt_api_search(query: str, limit: int, api_key: str) -> list[dict]:
    """Use search.list to get videoId / title / channelTitle. No descriptions."""
    qs = urllib.parse.urlencode({
        "part": "snippet",
        "type": "video",
        "maxResults": max(1, min(50, limit)),
        "q": query,
        "key": api_key,
    })
    url = f"{API_BASE}/search?{qs}"
    data = _http_get_json(url)
    out: list[dict] = []
    for it in data.get("items", []):
        vid = it.get("id", {}).get("videoId")
        sn = it.get("snippet", {}) or {}
        if not vid:
            continue
        out.append({
            "id": vid,
            "title": sn.get("title"),
            "uploader": sn.get("channelTitle"),
            "webpage_url": f"https://www.youtube.com/watch?v={vid}",
            "duration": None,
        })
    return out

def yt_api_durations(ids: list[str], api_key: str) -> dict[str, int | None]:
    """Batch videos.list(contentDetails) -> map id -> seconds."""
    out: dict[str, int | None] = {}
    base = f"{API_BASE}/videos"
    for i in range(0, len(ids), 50):
        chunk = ids[i:i+50]
        qs = urllib.parse.urlencode({
            "part": "contentDetails",
            "id": ",".join(chunk),
            "maxResults": 50,
            "key": api_key,
        })
        url = f"{base}?{qs}"
        data = _http_get_json(url)
        for it in data.get("items", []):
            vid = it.get("id")
            dur = it.get("contentDetails", {}).get("duration")
            out[vid] = _parse_iso8601_duration(dur)
        for vid in chunk:
            out.setdefault(vid, None)
    return out

def search_results(query: str, limit: int = 10, *, api_key: str | None = None) -> list[dict]:
    """Search via the YouTube Data API. Returns id/title/uploader/webpage_url/duration."""
    key = _require_api_key(api_key)
    results = yt_api_search(query, limit, key)
    if results:
        durs = yt_api_durations([r["id"] for r in results], key)
        for r in results:
            r["duration"] = durs.get(r["id"])
    return results

# ----------- Track folder files ----------

def _create_new(path: str) -> int:
    """Create path for writing; fails on an existing file or symlink."""
    return os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o644)

def _write_meta(track_dir: str, meta: dict):
    """Write <track_dir>/meta.json. It is the download's completion marker, so
    write it atomically and let failures propagate instead of reporting success."""
    tmp = os.path.join(track_dir, "meta.json.tmp")
    try:
        if stat.S_ISREG(os.lstat(tmp).st_mode):
            os.unlink(tmp)
    except FileNotFoundError:
        pass
    with os.fdopen(_create_new(tmp), "w", encoding="utf-8") as f:
        json.dump(meta, f, ensure_ascii=False, indent=2)
    os.replace(tmp, os.path.join(track_dir, "meta.json"))

def _track_meta(info_obj: dict) -> dict:
    return {
        "id": info_obj.get("id"),
        "title": info_obj.get("title"),
        "uploader": info_obj.get("uploader"),
        "duration": info_obj.get("duration"),
        "webpage_url": info_obj.get("webpage_url"),
    }

def _fetch_thumbnail(video_id: str, dest: str) -> bool:
    """Save YouTube's hqdefault.jpg for video_id to dest; False on any failure."""
    url = f"https://i.ytimg.com/vi/{video_id}/hqdefault.jpg"
    try:
        req = urllib.request.Request(
            url, headers={"User-Agent": "yplayer (https://github.com/HaoWen46/yplayer)"}
        )
        with urllib.request.urlopen(req, timeout=10) as resp:
            data = resp.read()
        with os.fdopen(_create_new(dest), "wb") as f:
            f.write(data)
    except Exception:
        return False
    return True

# ----------- Download ----------

# Time source for progress throttling; tests swap it for a fake clock.
_clock = time.monotonic

def download_track(url: str, cache_dir: str, *, emit, cancel_event) -> dict:
    """
    Download the native best-audio stream into <cache>/<Title> [<id8>]/ with one
    extract_info call. Streams {"event": "started", ...} through emit once audio
    bytes are on disk, then {"event": "progress", ...} at most twice per second.
    Setting cancel_event raises DownloadCancelled at the next progress callback.
    meta.json is written last; returns {path, dir, meta, thumb, format, file_size}.
    """
    track_tmpl = os.path.join(cache_dir, "%(title).150B [%(id).8s]")
    started = False
    last = 0.0
    job_dir = None

    def hook(d: dict):
        nonlocal started, last, job_dir
        path = d.get("filename")
        if path:
            job_dir = os.path.dirname(os.path.abspath(path))
        if cancel_event.is_set():
            raise DownloadCancelled()
        if not path:
            return
        if not started:
            try:
                on_disk = os.path.getsize(path)
            except OSError:
                return
            if not on_disk:
                return
            started, last = True, _clock()
            path = os.path.abspath(path)
            emit({
                "event": "started",
                "path": path,
                "dir": os.path.dirname(path),
                "meta": _track_meta(d.get("info_dict") or {}),
            })
            return
        now = _clock()
        if now - last < 0.5:
            return
        last = now
        total = d.get("total_bytes") or d.get("total_bytes_estimate")
        emit({
            "event": "progress",
            "bytes": d.get("downloaded_bytes") or 0,
            "total": int(total) if total else None,
        })

    ydl_opts = {
        "format": "bestaudio",
        "nopart": True,
        "outtmpl": {
            "default": os.path.join(track_tmpl, "audio.%(ext)s"),
        },
        # The direct https audio formats suffice; skip the HLS/DASH manifest fetches.
        "extractor_args": {"youtube": {"skip": ["hls", "dash"]}},
        "quiet": True,
        "no_warnings": True,
        # Keep stdout pure JSON for the worker protocol.
        "logtostderr": True,
        "noprogress": True,
        "noplaylist": True,
        "retries": 1,
        "extractor_retries": 1,
        "socket_timeout": 8,
        "progress_hooks": [hook],
    }
    try:
        with YoutubeDL(ydl_opts) as ydl:
            info_dict = ydl.extract_info(url, download=True)

        # yt-dlp drops keys equal to the parent's from requested_downloads; merge back.
        rd = {**info_dict, **info_dict["requested_downloads"][0]}
        path = os.path.abspath(rd["filepath"])
        track_dir = os.path.dirname(path)
        # Cover after the audio (yt-dlp's writethumbnail probes thumbnails serially
        # before the first audio byte); a missing cover is not an error.
        cover = os.path.join(track_dir, "cover.jpg")
        try:
            thumb = cover if _fetch_thumbnail(rd["id"], cover) else None
        except Exception:
            thumb = None
        meta = _track_meta(rd)
        _write_meta(track_dir, meta)
    except BaseException as e:
        if job_dir:
            e.job_dir = job_dir
        raise
    return {
        "path": path,
        "dir": track_dir,
        "meta": meta,
        "thumb": thumb,
        "format": os.path.splitext(path)[1].lstrip("."),
        "file_size": os.path.getsize(path),
    }

# ----------- Format inspection ----------

def list_audio_formats(url: str) -> list[dict]:
    """Inspect CDN audio formats for a specific URL using yt-dlp (heavy)."""
    ydl_opts = {
        "quiet": True,
        "skip_download": True,
        "noplaylist": True,
    }
    with YoutubeDL(ydl_opts) as ydl:
        d = ydl.extract_info(url, download=False)
    out = []
    for f in d.get("formats", []) or []:
        acodec = f.get("acodec")
        vcodec = f.get("vcodec")
        if acodec and acodec != "none" and (not vcodec or vcodec == "none"):
            out.append({
                "itag": f.get("format_id"),
                "ext": f.get("ext"),
                "abr": f.get("abr"),
                "asr": f.get("asr"),
                "filesize": f.get("filesize"),
                "format_note": f.get("format_note"),
            })
    return out
