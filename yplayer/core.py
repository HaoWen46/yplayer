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
- Sidecar JSON: legacy <cache>/<id>.json still written; folder meta.json added.
- Cached library listing helpers for the browse UI (both layouts).
"""
import os
import re
import json
import time
import urllib.parse
import urllib.request
import subprocess
import shutil
from dataclasses import dataclass
from typing import Dict, List, Optional, Set

from yt_dlp import YoutubeDL
from yt_dlp.utils import DownloadError

from .config import (
    DEFAULT_CACHE_DIR,
    DEFAULT_AUDIO_FORMAT,
    SUPPORTED_FORMATS,
    EMBED_METADATA_DEFAULT,
    KNOWN_EXTS,
    YTDL_AUDIO_FORMAT,
)
from .utils import which, die, info, normalize_ext
from .playback import Player

# ----------- URL / ID helpers -----------

YOUTUBE_URL_RE = re.compile(r"(https?://)?(www\.)?(youtube\.com|youtu\.be)/", re.I)
YOUTUBE_ID_RE = re.compile(r"(?:v=|\/)([0-9A-Za-z_-]{11})(?:[^0-9A-Za-z_-]|$)")

def is_url(s: str) -> bool:
    return bool(YOUTUBE_URL_RE.search(s))

def extract_video_id(url: str) -> Optional[str]:
    """Try to extract a YouTube video ID from common URL forms."""
    if not url:
        return None
    m = YOUTUBE_ID_RE.search(url)
    return m.group(1) if m else None

# ----------- Options -----------

@dataclass
class Options:
    cache_dir: str = DEFAULT_CACHE_DIR
    fmt: str = DEFAULT_AUDIO_FORMAT
    native: bool = False
    embed_meta: bool = EMBED_METADATA_DEFAULT
    audio_quality: Optional[str] = None  # "0" best for VBR when converting
    list_formats: bool = False
    player: Optional[str] = None
    play_after: bool = True
    volume: Optional[float] = None
    print_only: bool = False

# ----------- FS / deps -----------

def ensure_dir(path: str):
    os.makedirs(path, exist_ok=True)

def require_bins():
    """Warn if ffmpeg missing (yt-dlp library will still work for native downloads)."""
    if not which("ffmpeg") and not which("avconv"):
        info(
            "ffmpeg not found — native downloads will work, "
            "but conversion/metadata embedding won't.\n"
            "Install with: brew install ffmpeg"
        )

def _auto_update_ytdlp():
    """Try to self-update the yt-dlp CLI if present (used as fallback when DownloadError occurs)."""
    ytdlp_bin = which("yt-dlp")
    if not ytdlp_bin:
        return
    try:
        info("attempting to update yt-dlp…")
        subprocess.run([ytdlp_bin, "-U"], check=True)
        info("yt-dlp updated successfully.")
    except Exception as e:
        info(f"yt-dlp update failed: {e}")

# ----------- YouTube Data API (no descriptions) ----------

API_BASE = "https://www.googleapis.com/youtube/v3"

def _require_api_key(api_key: Optional[str]) -> str:
    key = api_key or os.environ.get("YT_API_KEY")
    if not key:
        die("YouTube Data API key missing. Set $YT_API_KEY or pass --yt-api-key.")
    return key

def _http_get_json(url: str, timeout: int = 10) -> Dict:
    req = urllib.request.Request(url, headers={"Accept": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))

def _parse_iso8601_duration(s: Optional[str]) -> Optional[int]:
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

def yt_api_search(query: str, limit: int, api_key: str) -> List[Dict]:
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
    out: List[Dict] = []
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

def yt_api_durations(ids: List[str], api_key: str) -> Dict[str, Optional[int]]:
    """Batch videos.list(contentDetails) -> map id -> seconds."""
    out: Dict[str, Optional[int]] = {}
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

def yt_api_video_info(video_id: str, api_key: str) -> Optional[Dict]:
    """Fetch minimal info for a single video: title, uploader, duration."""
    qs = urllib.parse.urlencode({
        "part": "snippet,contentDetails",
        "id": video_id,
        "key": api_key,
    })
    url = f"{API_BASE}/videos?{qs}"
    data = _http_get_json(url)
    items = data.get("items", [])
    if not items:
        return None
    it = items[0]
    sn = it.get("snippet", {}) or {}
    cd = it.get("contentDetails", {}) or {}
    return {
        "id": video_id,
        "title": sn.get("title") or video_id,
        "uploader": sn.get("channelTitle"),
        "duration": _parse_iso8601_duration(cd.get("duration")),
        "webpage_url": f"https://www.youtube.com/watch?v={video_id}",
    }

# ----------- Metadata sidecar & cache listing ----------

def _meta_path(cache_dir: str, vid: str) -> str:
    # legacy flat sidecar path
    return os.path.join(cache_dir, f"{vid}.json")

def save_sidecar(cache_dir: str, info_obj: Dict, *, track_dir: Optional[str] = None):
    """Write minimal metadata JSON next to the audio file.
       Writes both the legacy <cache>/<id>.json and, if track_dir provided, <track_dir>/meta.json.
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
    # legacy
    try:
        with open(_meta_path(cache_dir, vid), "w", encoding="utf-8") as f:
            json.dump(meta, f, ensure_ascii=False, indent=2)
    except Exception:
        pass
    # per-track
    if track_dir:
        try:
            os.makedirs(track_dir, exist_ok=True)
            with open(os.path.join(track_dir, "meta.json"), "w", encoding="utf-8") as f:
                json.dump(meta, f, ensure_ascii=False, indent=2)
        except Exception:
            pass

def _sanitize_title(title: Optional[str]) -> str:
    """Make a filesystem-safe-ish filename from a title (keeps unicode)."""
    if not title:
        return ""
    s = title.strip()
    s = re.sub(r'[:\/\\\?\*"<>\|\n\r\t]', '', s)
    s = re.sub(r'\s+', ' ', s)
    return s[:200].strip()

def _pick_existing_path(cache_dir: str, vid: str) -> Optional[str]:
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

def _first_audio_in_dir(d: str) -> Optional[str]:
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

def find_existing(cache_dir: str, vid: str, title: Optional[str] = None) -> Optional[str]:
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
            with open(os.path.join(d, "meta.json"), "r", encoding="utf-8") as f:
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

def list_cached_tracks(cache_dir: str) -> List[Dict]:
    """Scan cache dir, return unique tracks with sidecar metadata if present. Supports both layouts."""
    out: List[Dict] = []
    if not os.path.isdir(cache_dir):
        return out

    # per-track folders
    for d in _iter_track_dirs(cache_dir):
        meta = None
        try:
            with open(os.path.join(d, "meta.json"), "r", encoding="utf-8") as f:
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
    seen_ids: Dict[str, Dict] = {}
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
                    with open(full, "r", encoding="utf-8") as f:
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

def list_cached_playlists(cache_dir: str) -> List[Dict]:
    """Return cached playlist manifests as browse-items."""
    out = []
    if not os.path.isdir(cache_dir):
        return out

    from .playlist import PlaylistManifest  # local import to avoid cycle
    for name in os.listdir(cache_dir):
        if not name.endswith(".plist.json"):
            continue
        try:
            m = PlaylistManifest.load(os.path.join(cache_dir, name))
            out.append(
                {
                    "type": "playlist",
                    "title": m.title,
                    "id": m.id,
                    "count": len(m.tracks),
                    "path": os.path.join(cache_dir, name),
                }
            )
        except Exception:
            pass
    return out

# ----------- YTDL helpers (download only) ----------

# Recognise playlist URLs
_PLAYLIST_RE = re.compile(r"[?&]list=([a-zA-Z0-9_-]{10,})")
def is_playlist_url(url: str) -> bool:
    return bool(_PLAYLIST_RE.search(url))

def _base_ydl_opts(cache_dir: str) -> Dict:
    return {
        "quiet": True,
        "no_warnings": True,
        "noplaylist": True,
        "outtmpl": os.path.join(cache_dir, "%(id)s.%(ext)s"),
        "format": "bestaudio/best",
        "retries": 2,
        "socket_timeout": 10,
    }

def _ydl_extract(url_or_query: str, ydl_opts: Dict, *, download: bool):
    """Wrapper around YoutubeDL.extract_info with optional auto-update and retry."""
    try:
        with YoutubeDL(ydl_opts) as ydl:
            return ydl.extract_info(url_or_query, download=download)
    except DownloadError:
        _auto_update_ytdlp()
        with YoutubeDL(ydl_opts) as ydl:
            return ydl.extract_info(url_or_query, download=download)

# ----------- Download (per-track folder layout) ----------

def path_for(cache_dir: str, vid: str, ext: str) -> str:
    return os.path.join(cache_dir, f"{vid}.{normalize_ext(ext)}")

def _track_dir_name(title: Optional[str], vid: Optional[str]) -> str:
    san_title = _sanitize_title(title) if title else None
    base = san_title or (vid or "track")
    if vid:
        base = f"{base} [{vid[:8]}]"
    return base

def _first_audio_created(before: Set[str], after: Set[str], directory: str) -> Optional[str]:
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

def download_audio(url: str, opts: Options, *, api_key: Optional[str] = None) -> str:
    """
    Download audio and return the actual file path.
    New behavior: per-track folder layout. Legacy flat-cache still compatible.
    """
    ensure_dir(opts.cache_dir)

    # 1) Minimal metadata via API if available
    vid = None
    title = None
    uploader = None
    duration = None
    try:
        if api_key or os.environ.get("YT_API_KEY"):
            try:
                info_obj = video_info_from_url(url, api_key=api_key)
                vid = info_obj.get("id")
                title = info_obj.get("title")
                uploader = info_obj.get("uploader")
                duration = info_obj.get("duration")
            except Exception:
                vid = extract_video_id(url)
        else:
            vid = extract_video_id(url)
    except Exception:
    # be tolerant
        vid = extract_video_id(url)

    # 2) Prepare per-track directory and outtmpl
    tdir_name = _track_dir_name(title, vid)
    tdir = os.path.join(opts.cache_dir, tdir_name)
    os.makedirs(tdir, exist_ok=True)
    outtmpl = os.path.join(tdir, "audio.%(ext)s")

    ydl_opts = {
        "quiet": True,
        "no_warnings": True,
        "noplaylist": True,
        "outtmpl": outtmpl,
        "format": "bestaudio/best",
        "retries": 2,
        "socket_timeout": 10,
        "restrictfilenames": True,
    }

    if not opts.native:
        fmt = normalize_ext(opts.fmt)
        if fmt not in SUPPORTED_FORMATS:
            die(f"unsupported format: {fmt} (supported: {', '.join(SUPPORTED_FORMATS)})")
        ydl_opts["postprocessors"] = [
            {"key": "FFmpegExtractAudio", "preferredcodec": fmt, "preferredquality": str(opts.audio_quality or "0")}
        ]
        if opts.embed_meta:
            ydl_opts["writethumbnail"] = True
            ydl_opts["postprocessors"].extend([{"key": "FFmpegMetadata"}, {"key": "EmbedThumbnail"}])

    info("downloading audio-only…")

    # snapshot files inside tdir before download
    before: Set[str] = set(os.listdir(tdir)) if os.path.isdir(tdir) else set()
    info_dict = _ydl_extract(url, ydl_opts, download=True)
    after: Set[str] = set(os.listdir(tdir)) if os.path.isdir(tdir) else set()

    # 3) Determine final path
    final_path: Optional[str] = None
    # yt-dlp may populate requested_downloads
    if isinstance(info_dict, dict) and info_dict.get("requested_downloads"):
        rd = info_dict["requested_downloads"][0]
        final_path = rd.get("filepath")
    if not final_path:
        # look for new audio in tdir
        for fname in sorted(after - before):
            ext = os.path.splitext(fname)[1].lstrip(".").lower()
            if ext in KNOWN_EXTS:
                final_path = os.path.join(tdir, fname)
                break
    if not final_path:
        # fallback to expected template
        ext = opts.fmt if not opts.native else (info_dict.get("ext") or "webm")
        final_path = os.path.join(tdir, f"audio.{ext}")

    # 4) Write sidecars (legacy + per-track)
    vid_final = info_dict.get("id") or vid or extract_video_id(url)
    meta = {
        "id": vid_final,
        "title": title or info_dict.get("title") or vid_final,
        "uploader": uploader or info_dict.get("uploader"),
        "duration": duration or info_dict.get("duration"),
        "webpage_url": info_dict.get("webpage_url") or url,
    }
    save_sidecar(opts.cache_dir, meta, track_dir=tdir)

    return final_path

# ----------- Inspect / search (API-first) ----------

def search_results(query: str, limit: int = 10, *, api_key: Optional[str] = None,
                   want_duration: bool = True) -> List[Dict]:
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

def video_info_from_url(url: str, *, api_key: Optional[str] = None) -> Dict:
    """Minimal info for a single URL using YouTube Data API. No descriptions."""
    key = _require_api_key(api_key)
    vid = extract_video_id(url)
    if not vid:
        die("could not extract video id from URL")
    info_obj = yt_api_video_info(vid, key)
    if not info_obj:
        die("video not found via API")
    return info_obj

def video_info_from_query(query: str, *, api_key: Optional[str] = None) -> Dict:
    """Top-1 result via API (kept for completeness)."""
    res = search_results(query, limit=1, api_key=api_key, want_duration=True)
    if not res:
        die("no results")
    return res[0]

def list_audio_formats(url: str) -> List[Dict]:
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

# ----------- Orchestration ----------

def resolve_and_maybe_download(query_or_url: str, opts: Options, *, api_key: Optional[str] = None) -> str:
    ensure_dir(opts.cache_dir)

    if is_url(query_or_url):
        info_obj = video_info_from_url(query_or_url, api_key=api_key)
    else:
        die("refusing to download from a search query. pass a YouTube URL.")

    vid = info_obj["id"]

    existing = find_existing(opts.cache_dir, vid, info_obj.get("title"))
    if existing:
        info(f"cached: {os.path.basename(existing)}")
        return existing

    # Save both sidecars before download (so browse shows it even during download)
    try:
        # we don't have the per-track dir yet; download_audio will add folder meta
        save_sidecar(opts.cache_dir, info_obj)
    except Exception:
        pass

    return download_audio(info_obj["webpage_url"], opts, api_key=api_key)

def run_and_maybe_play(query_or_url: str, opts: Options, *, api_key: Optional[str] = None) -> str:
    path = resolve_and_maybe_download(query_or_url, opts, api_key=api_key)
    if opts.play_after and not opts.print_only:
        player = Player(prefer=opts.player)
        player.play(path, volume=opts.volume)
    return path
