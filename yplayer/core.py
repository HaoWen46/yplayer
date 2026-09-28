# yplayer/core.py
"""
Full, drop-in core module (patched for per-track folders).

Features:
- Metadata (search / durations / single-video info) via YouTube Data API (no descriptions).
- Downloads via yt-dlp.
- **New:** per-track folder layout for new downloads:
    ~/.cache/yplayer/<SanitizedTitle> [<id8>]/audio.<ext>
    ~/.cache/yplayer/<SanitizedTitle> [<id8>]/meta.json
  (Legacy flat cache remains supported.)
- Robust post-download discovery of actual filename.
- Sidecar JSON: folder meta.json, written after the audio.
- Cached library listing helpers for the browse UI (both layouts).
"""
import json
import os
import re
import stat
import time
import urllib.parse
import urllib.request

from yt_dlp import YoutubeDL
from yt_dlp.utils import DownloadCancelled

# DEFAULT_CACHE_DIR is re-exported for albums.py.
from .config import DEFAULT_CACHE_DIR as DEFAULT_CACHE_DIR
from .config import KNOWN_EXTS
from .utils import die, info, normalize_ext, which

# ----------- FS / deps -----------

def ensure_dir(path: str):
    os.makedirs(path, exist_ok=True)

def require_bins():
    """Warn if ffmpeg missing."""
    if not which("ffmpeg") and not which("avconv"):
        info(
            "ffmpeg not found — native downloads will work, "
            "but conversion/metadata embedding won't.\n"
            "Install with: brew install ffmpeg"
        )

# ----------- YouTube Data API (no descriptions) ----------

API_BASE = "https://www.googleapis.com/youtube/v3"

def _require_api_key(api_key: str | None) -> str:
    key = api_key or os.environ.get("YT_API_KEY")
    if not key:
        die("YouTube Data API key missing. Set $YT_API_KEY or pass --yt-api-key.")
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

# ----------- Metadata sidecar & cache listing ----------

def _create_new(path: str) -> int:
    """Create path for writing; fails on an existing file or symlink."""
    return os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o644)

def save_sidecar(cache_dir: str, info_obj: dict, *, track_dir: str | None = None):
    """Write minimal metadata JSON next to the audio file.
       Writes <track_dir>/meta.json if track_dir provided.
    """
    ensure_dir(cache_dir)
    vid = info_obj.get("id")
    if not vid:
        return
    meta = {
        "id": vid,
        "title": info_obj.get("title"),
        "uploader": info_obj.get("uploader"),
        "duration": info_obj.get("duration"),
        "webpage_url": info_obj.get("webpage_url"),
    }
    # per-track. meta.json is the download's completion marker, so write it
    # atomically and let failures propagate instead of reporting success.
    if track_dir:
        os.makedirs(track_dir, exist_ok=True)
        tmp = os.path.join(track_dir, "meta.json.tmp")
        try:
            if stat.S_ISREG(os.lstat(tmp).st_mode):
                os.unlink(tmp)
        except FileNotFoundError:
            pass
        with os.fdopen(_create_new(tmp), "w", encoding="utf-8") as f:
            json.dump(meta, f, ensure_ascii=False, indent=2)
        os.replace(tmp, os.path.join(track_dir, "meta.json"))

def _sanitize_title(title: str | None) -> str:
    """Make a filesystem-safe-ish filename from a title (keeps unicode)."""
    if not title:
        return ""
    s = title.strip()
    s = re.sub(r'[:\/\\\?\*"<>\|\n\r\t]', '', s)
    s = re.sub(r'\s+', ' ', s)
    return s[:200].strip()

def _pick_existing_path(cache_dir: str, vid: str) -> str | None:
    """Find an existing file by id or fuzzy matches in flat layout."""
    # 1) id.ext exact
    for ext in KNOWN_EXTS:
        p = os.path.join(cache_dir, f"{vid}.{ext}")
        if os.path.exists(p):
            return p
    # 2) filename contains id
    try:
        for fname in os.listdir(cache_dir):
            if vid in fname:
                ext = os.path.splitext(fname)[1].lstrip(".").lower()
                if ext in KNOWN_EXTS:
                    return os.path.join(cache_dir, fname)
    except Exception:
        pass
    return None

def _iter_track_dirs(cache_dir: str):
    try:
        for name in os.listdir(cache_dir):
            d = os.path.join(cache_dir, name)
            if os.path.isdir(d) and os.path.exists(os.path.join(d, "meta.json")):
                yield d
    except Exception:
        return

def _first_audio_in_dir(d: str) -> str | None:
    try:
        for fname in os.listdir(d):
            p = os.path.join(d, fname)
            if os.path.isfile(p):
                ext = os.path.splitext(fname)[1].lstrip(".").lower()
                if ext in KNOWN_EXTS:
                    return p
    except Exception:
        return None
    return None

def find_existing(cache_dir: str, vid: str, title: str | None = None) -> str | None:
    """
    Search cache_dir for a file matching the video id or the title (sanitized).
    Supports both layouts (per-track folder and legacy flat).
    Returns full path or None.
    """
    if not cache_dir or not vid:
        return None

    # 0) per-track folder: look for meta.json where id matches
    for d in _iter_track_dirs(cache_dir):
        try:
            with open(os.path.join(d, "meta.json"), encoding="utf-8") as f:
                meta = json.load(f)
            if meta.get("id") == vid:
                p = _first_audio_in_dir(d)
                if p:
                    return p
        except Exception:
            continue

    # 1) exact id-based files (legacy flat)
    exact = _pick_existing_path(cache_dir, vid)
    if exact:
        return exact

    # 2) title-based attempts (legacy flat)
    if title:
        san = _sanitize_title(title)
        if san:
            # exact sanitized match
            for ext in KNOWN_EXTS:
                cand = os.path.join(cache_dir, f"{san}.{ext}")
                if os.path.exists(cand):
                    return cand
            # startswith / contains match
            try:
                san_l = san.lower()
                for fname in os.listdir(cache_dir):
                    name_noext = os.path.splitext(fname)[0].lower()
                    if name_noext.startswith(san_l) or san_l in name_noext:
                        ext = os.path.splitext(fname)[1].lstrip(".").lower()
                        if ext in KNOWN_EXTS:
                            return os.path.join(cache_dir, fname)
            except Exception:
                pass

    return None

def list_cached_tracks(cache_dir: str) -> list[dict]:
    """Scan cache dir, return unique tracks with sidecar metadata if present. Supports both layouts."""
    out: list[dict] = []
    if not os.path.isdir(cache_dir):
        return out

    # per-track folders
    for d in _iter_track_dirs(cache_dir):
        meta = None
        try:
            with open(os.path.join(d, "meta.json"), encoding="utf-8") as f:
                meta = json.load(f)
        except Exception:
            meta = None
        audio = _first_audio_in_dir(d)
        if audio:
            out.append({
                "id": (meta or {}).get("id"),
                "title": (meta or {}).get("title") or os.path.basename(d),
                "uploader": (meta or {}).get("uploader"),
                "duration": (meta or {}).get("duration"),
                "webpage_url": (meta or {}).get("webpage_url"),
                "path": audio,
            })

    # legacy flat files + sidecars
    seen_ids: dict[str, dict] = {}
    try:
        for name in os.listdir(cache_dir):
            base, ext = os.path.splitext(name)
            ext = ext.lstrip(".").lower()
            full = os.path.join(cache_dir, name)
            if ext in KNOWN_EXTS:
                vid = base
                entry = seen_ids.setdefault(vid, {})
                entry["path"] = full
            elif ext == "json":
                try:
                    with open(full, encoding="utf-8") as f:
                        meta = json.load(f)
                    vid = meta.get("id") or base
                    entry = seen_ids.setdefault(vid, {})
                    entry.update({
                        "id": vid,
                        "title": meta.get("title"),
                        "uploader": meta.get("uploader"),
                        "duration": meta.get("duration"),
                        "webpage_url": meta.get("webpage_url"),
                    })
                except Exception:
                    pass
    except Exception:
        pass

    for vid, d in seen_ids.items():
        path = d.get("path") or _pick_existing_path(cache_dir, vid)
        if not path:
            continue
        d.setdefault("id", vid)
        d["path"] = path
        out.append(d)

    out.sort(key=lambda x: (x.get("title") or os.path.basename(x["path"])).lower())
    return out

# ----------- YTDL helpers (download only) ----------

# Recognise playlist URLs
_PLAYLIST_RE = re.compile(r"[?&]list=([a-zA-Z0-9_-]{10,})")
def is_playlist_url(url: str) -> bool:
    return bool(_PLAYLIST_RE.search(url))

def _base_ydl_opts(cache_dir: str) -> dict:
    return {
        "quiet": True,
        "no_warnings": True,
        # Keep stdout pure JSON for the worker protocol: send all yt-dlp output
        # (incl. the download progress bar) to stderr, and disable progress.
        "logtostderr": True,
        "noprogress": True,
        "noplaylist": True,
        "outtmpl": os.path.join(cache_dir, "%(id)s.%(ext)s"),
        "format": "bestaudio/best",
        "retries": 2,
        "socket_timeout": 10,
    }

# ----------- Download (per-track folder layout) ----------

def path_for(cache_dir: str, vid: str, ext: str) -> str:
    return os.path.join(cache_dir, f"{vid}.{normalize_ext(ext)}")

def _first_audio_created(before: set[str], after: set[str], directory: str) -> str | None:
    # Find new audio file created in directory
    try:
        new_files = list(set(os.listdir(directory)) - (before if directory == "." else set()))
    except Exception:
        new_files = []
    for fname in new_files:
        ext = os.path.splitext(fname)[1].lstrip(".").lower()
        if ext in KNOWN_EXTS:
            return os.path.join(directory, fname)
    return None

# Time source for progress throttling; tests swap it for a fake clock.
_clock = time.monotonic

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
        save_sidecar(cache_dir, meta, track_dir=track_dir)
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

# ----------- Inspect / search (API-first) ----------

def search_results(query: str, limit: int = 10, *, api_key: str | None = None,
                   want_duration: bool = True) -> list[dict]:
    """Fast search via YouTube Data API. Returns id/title/uploader/webpage_url/duration."""
    key = _require_api_key(api_key) if want_duration or api_key else (api_key or os.environ.get("YT_API_KEY"))
    if want_duration:
        key = _require_api_key(api_key)
    results = yt_api_search(query, limit, key) if key else yt_api_search(query, limit, os.environ.get("YT_API_KEY", ""))
    if want_duration and results:
        ids = [r["id"] for r in results]
        durs = yt_api_durations(ids, key)
        for r in results:
            r["duration"] = durs.get(r["id"])
    return results

def video_info_from_query(query: str, *, api_key: str | None = None) -> dict:
    """Top-1 result via API (kept for completeness)."""
    res = search_results(query, limit=1, api_key=api_key, want_duration=True)
    if not res:
        die("no results")
    return res[0]

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
