"""
JSON-line worker for the yplayer service (protocol v2).

Protocol: reads one JSON object per line from stdin, writes one JSON object
per line to stdout.  stderr is used for logging (appended to the worker log
by the Rust side).  The first line is {"event": "ready", "ok": true,
"protocol": 2}; every later line carries the request id.  Requests run on a
thread pool, so replies can interleave; each request ends with exactly one
terminal line ({"ok": true, ...} or {"ok": false, "error", "cancelled"}).

Commands:
    download        – download native audio; streams started/progress events
    cancel          – cancel an in-flight download (handled inline)
    search          – search YouTube, return results
    playlist_entries – extract playlist entries (flat, no download)
    list_formats    – list available audio formats for a URL
"""

import json
import os
import sys
import threading
from concurrent.futures import ThreadPoolExecutor

from yt_dlp.utils import DownloadCancelled

from .core import download_track, list_audio_formats, search_results
from .playlist import extract_playlist_entries

# Serializes stdout writes from the reader thread and the pool threads.
_out_lock = threading.Lock()

# Cancel flags of in-flight requests, keyed by request id.
_cancel_events: dict = {}


def _respond(obj: dict):
    """Write a JSON line to stdout and flush."""
    line = json.dumps(obj, ensure_ascii=False) + "\n"
    with _out_lock:
        sys.stdout.write(line)
        sys.stdout.flush()


def _handle_download(req: dict, emit, cancel_event) -> dict:
    cache_dir = req.get("cache_dir") or os.path.expanduser("~/Music/yt-audio")
    url = req.get("url", "")
    return download_track(url, cache_dir, emit=emit, cancel_event=cancel_event)


def _handle_search(req: dict, emit, cancel_event) -> dict:
    query = req.get("query", "")
    limit = req.get("limit", 10)
    api_key = req.get("api_key")

    results = search_results(query, limit, api_key=api_key)
    return {"results": results}


def _handle_playlist_entries(req: dict, emit, cancel_event) -> dict:
    url = req.get("url", "")
    entries = extract_playlist_entries(url)
    return {"entries": entries}


def _handle_list_formats(req: dict, emit, cancel_event) -> dict:
    url = req.get("url", "")
    formats = list_audio_formats(url)
    return {"formats": formats}


def _run(handler, rid, req: dict, cancel_event: threading.Event):
    """Run one request on a pool thread and write its terminal line."""

    def emit(event: dict):
        _respond({"id": rid, **event})

    def failed(error: str, cancelled: bool, e: BaseException):
        reply = {"id": rid, "ok": False, "error": error, "cancelled": cancelled}
        job_dir = getattr(e, "job_dir", None)
        if job_dir:
            reply["dir"] = job_dir
        _respond(reply)

    try:
        emit({"event": "running"})
        result = handler(req, emit, cancel_event)
        _respond({"id": rid, "ok": True, **result})
    except SystemExit as e:
        # A library may call sys.exit(); keep the worker alive and report it.
        failed(f"worker aborted: {e}", False, e)
    except Exception as e:
        # Includes YplayerError from die(), which carries the real message.
        failed(str(e), isinstance(e, DownloadCancelled), e)
    finally:
        _cancel_events.pop(rid, None)


def main():
    handlers = {
        "download": _handle_download,
        "search": _handle_search,
        "playlist_entries": _handle_playlist_entries,
        "list_formats": _handle_list_formats,
    }

    # Announce readiness so the host can tell a live worker from one that failed
    # to import/start, and can begin sending requests.
    _respond({"event": "ready", "ok": True, "protocol": 2})

    # Leaving the block on stdin EOF waits for running jobs; then exit 0.
    with ThreadPoolExecutor(max_workers=4) as pool:
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            try:
                req = json.loads(line)
            except json.JSONDecodeError as e:
                _respond(
                    {"id": None, "ok": False, "error": f"Invalid JSON: {e}", "cancelled": False}
                )
                continue

            rid = req.get("id")
            cmd = req.get("cmd", "")
            if cmd == "cancel":
                event = _cancel_events.get(req.get("target"))
                if event is not None:
                    event.set()
                _respond({"id": rid, "ok": True})
                continue

            handler = handlers.get(cmd)
            if handler is None:
                _respond(
                    {"id": rid, "ok": False, "error": f"Unknown command: {cmd}", "cancelled": False}
                )
                continue

            cancel_event = threading.Event()
            _cancel_events[rid] = cancel_event
            pool.submit(_run, handler, rid, req, cancel_event)


if __name__ == "__main__":
    main()
