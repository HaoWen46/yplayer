"""
JSON-line worker for the Rust yplay binary.

Protocol: reads one JSON object per line from stdin, writes one JSON object
per line to stdout.  stderr is used for progress/logging (passed through to
the terminal by the Rust side).

Commands:
    download        – download audio, return file path + metadata
    search          – search YouTube, return results
    playlist_entries – extract playlist entries (flat, no download)
    video_info      – fetch metadata for a single URL
    list_formats    – list available audio formats for a URL
"""

import json
import sys
import os

# Ensure yt-dlp is up to date on worker start
from .core import (
    ensure_ytdlp_uptodate,
    require_bins,
    download_audio,
    search_results,
    video_info_from_url,
    list_audio_formats,
    Options,
)
from .playlist import extract_playlist_entries


def _respond(obj: dict):
    """Write a JSON line to stdout and flush."""
    sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def _handle_download(req: dict):
    cache_dir = req.get("cache_dir") or os.path.expanduser("~/Music/yt-audio")
    url = req.get("url", "")
    api_key = req.get("api_key")
    fmt = req.get("format", "mp3")
    native = req.get("native", False)
    embed_meta = req.get("embed_meta", True)

    opts = Options()
    opts.cache_dir = cache_dir
    opts.fmt = fmt
    opts.native = native
    opts.embed_meta = embed_meta
    opts.play_after = False

    path = download_audio(url, opts, api_key=api_key)

    # Read the sidecar metadata if available
    meta = {}
    try:
        meta_path = os.path.join(os.path.dirname(path), "meta.json")
        if os.path.exists(meta_path):
            with open(meta_path, "r", encoding="utf-8") as f:
                meta = json.load(f)
    except Exception:
        pass

    if not meta:
        from .core import extract_video_id
        vid = extract_video_id(url) or ""
        meta = {"id": vid, "title": vid, "webpage_url": url}

    _respond({"ok": True, "path": path, "meta": meta})


def _handle_search(req: dict):
    query = req.get("query", "")
    limit = req.get("limit", 10)
    api_key = req.get("api_key")

    results = search_results(query, limit, api_key=api_key)
    _respond({"ok": True, "results": results})


def _handle_video_info(req: dict):
    url = req.get("url", "")
    api_key = req.get("api_key")

    info = video_info_from_url(url, api_key=api_key)
    _respond({"ok": True, "meta": info})


def _handle_playlist_entries(req: dict):
    url = req.get("url", "")
    entries = extract_playlist_entries(url)
    _respond({"ok": True, "entries": entries})


def _handle_list_formats(req: dict):
    url = req.get("url", "")
    formats = list_audio_formats(url)
    _respond({"ok": True, "formats": formats})


def main():
    # Auto-update yt-dlp on worker start
    try:
        ensure_ytdlp_uptodate()
    except Exception:
        pass

    handlers = {
        "download": _handle_download,
        "search": _handle_search,
        "video_info": _handle_video_info,
        "playlist_entries": _handle_playlist_entries,
        "list_formats": _handle_list_formats,
    }

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError as e:
            _respond({"ok": False, "error": f"Invalid JSON: {e}"})
            continue

        cmd = req.get("cmd", "")
        handler = handlers.get(cmd)
        if handler is None:
            _respond({"ok": False, "error": f"Unknown command: {cmd}"})
            continue

        try:
            handler(req)
        except SystemExit:
            # core.die() calls sys.exit — catch and report as error
            _respond({"ok": False, "error": "Operation failed (see stderr)"})
        except Exception as e:
            _respond({"ok": False, "error": str(e)})


if __name__ == "__main__":
    main()
