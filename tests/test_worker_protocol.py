"""Worker protocol v2 tests.

An in-process harness runs ``worker.main()`` on a thread over fake stdin/stdout,
with ``yt_dlp.YoutubeDL`` replaced by a scripted fake that writes a real file
under a temp cache dir and drives the progress hooks chunk by chunk.
"""
import json
import os
import queue
import subprocess
import sys
import threading
import time
from types import SimpleNamespace

import pytest
from yt_dlp.utils import DownloadError

from yplayer import core, worker

TIMEOUT = 5
CHUNK = 1024

_real_fetch_thumbnail = core._fetch_thumbnail


class FakeStdin:
    def __init__(self):
        self._q = queue.Queue()

    def send(self, obj: dict):
        self._q.put(json.dumps(obj) + "\n")

    def close(self):
        self._q.put(None)

    def __iter__(self):
        while (line := self._q.get()) is not None:
            yield line


class FakeStdout:
    def __init__(self):
        self.lines: list[dict] = []
        self._buf = ""
        self._cond = threading.Condition()

    def write(self, s: str):
        with self._cond:
            self._buf += s
            *done, self._buf = self._buf.split("\n")
            self.lines.extend(json.loads(ln) for ln in done if ln)
            self._cond.notify_all()

    def flush(self):
        pass

    def wait_for(self, pred) -> dict:
        with self._cond:
            ok = self._cond.wait_for(lambda: any(pred(m) for m in self.lines), TIMEOUT)
            assert ok, f"timed out; lines so far: {self.lines!r}"
            return next(m for m in self.lines if pred(m))

    def terminal(self, rid) -> dict:
        return self.wait_for(lambda m: m.get("id") == rid and "ok" in m)

    def for_id(self, rid) -> list[dict]:
        return [m for m in self.lines if m.get("id") == rid]


class FakeYDL:
    """Stands in for yt_dlp.YoutubeDL; behaviour is scripted per URL."""

    scripts: dict[str, dict] = {}

    def __init__(self, params: dict):
        self.params = params

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False

    def _hook(self, d: dict):
        for hook in self.params.get("progress_hooks", []):
            hook(d)

    def extract_info(self, url: str, download: bool = True) -> dict:
        s = self.scripts[url]
        s["params"] = self.params
        if s.get("error"):
            raise DownloadError(s["error"])
        info = {
            "id": s["vid"],
            "title": s["title"],
            "uploader": "Uploader",
            "duration": 212,
            "webpage_url": url,
            "ext": "webm",
        }

        path = (
            self.params["outtmpl"]["default"]
            .replace("%(title).150B", info["title"])
            .replace("%(id).8s", info["id"][:8])
            .replace("%(ext)s", "webm")
        )
        track_dir = os.path.dirname(path)
        os.makedirs(track_dir, exist_ok=True)

        chunks = s.get("chunks", 3)
        with open(path, "wb") as f:
            for k in range(1, chunks + 1):
                f.write(b"\0" * CHUNK)
                f.flush()
                if s.get("tick"):
                    s["tick"](k)
                self._hook({
                    "status": "downloading",
                    "downloaded_bytes": k * CHUNK,
                    "total_bytes": chunks * CHUNK,
                    "filename": path,
                    "info_dict": info,
                })
                if k == 1 and s.get("gate"):
                    assert s["gate"].wait(TIMEOUT)
                if k == 1 and s.get("barrier"):
                    s["barrier"].wait()
        self._hook({
            "status": "finished",
            "downloaded_bytes": chunks * CHUNK,
            "total_bytes": chunks * CHUNK,
            "filename": path,
            "info_dict": info,
        })
        # meta.json must not exist until the audio is complete.
        s["meta_during_download"] = os.path.exists(os.path.join(track_dir, "meta.json"))
        # Like yt-dlp: requested_downloads carries filepath, not keys equal to the parent's.
        return {**info, "requested_downloads": [{"filepath": path, "format_id": "251"}]}


def _fake_fetch_thumbnail(video_id: str, dest: str) -> bool:
    with open(dest, "wb") as f:
        f.write(b"jpg")
    return True


@pytest.fixture
def harness(monkeypatch, tmp_path):
    stdin, stdout = FakeStdin(), FakeStdout()
    # Patch the worker's own `sys` reference: pytest's capture re-points the
    # real sys.stdout between fixture setup and the test call.
    monkeypatch.setattr(worker, "sys", SimpleNamespace(stdin=stdin, stdout=stdout))
    monkeypatch.setattr(core, "YoutubeDL", FakeYDL)
    monkeypatch.setattr(FakeYDL, "scripts", {})
    monkeypatch.setattr(core, "_fetch_thumbnail", _fake_fetch_thumbnail)
    thread =threading.Thread(target=worker.main, daemon=True)
    thread.start()
    yield SimpleNamespace(
        stdin=stdin, stdout=stdout, thread=thread, cache=str(tmp_path), scripts=FakeYDL.scripts
    )
    stdin.close()
    thread.join(TIMEOUT)


def _download(h, rid: int, url: str):
    h.stdin.send({"id": rid, "cmd": "download", "url": url, "cache_dir": h.cache})


def test_ready_line_has_protocol_2(harness):
    first = harness.stdout.wait_for(lambda m: True)
    assert first == {"event": "ready", "ok": True, "protocol": 2}


def test_download_streams_started_then_throttled_progress_then_ok(harness, monkeypatch):
    clock = SimpleNamespace(t=0.0)
    monkeypatch.setattr(core, "_clock", lambda: clock.t)
    url = "https://www.youtube.com/watch?v=AAAAAAAAAAA"
    # Chunk k lands at t = 0.25 * k: unthrottled that is 9 progress lines after started.
    harness.scripts[url] = {
        "vid": "AAAAAAAAAAA",
        "title": "Song",
        "chunks": 10,
        "tick": lambda k: setattr(clock, "t", 0.25 * k),
    }
    _download(harness, 1, url)
    done = harness.stdout.terminal(1)

    msgs = harness.stdout.for_id(1)
    assert [m.get("event") for m in msgs] == ["running", "started"] + ["progress"] * 4 + [None]
    assert msgs[0] == {"id": 1, "event": "running"}
    started = msgs[1]
    track_dir = os.path.join(harness.cache, "Song [AAAAAAAA]")
    audio = os.path.join(track_dir, "audio.webm")
    meta = {
        "id": "AAAAAAAAAAA",
        "title": "Song",
        "uploader": "Uploader",
        "duration": 212,
        "webpage_url": url,
    }
    assert started == {"id": 1, "event": "started", "path": audio, "dir": track_dir, "meta": meta}
    # At most one progress line per 0.5 s of (fake) time: chunks 3, 5, 7, 9.
    assert [(m["bytes"], m["total"]) for m in msgs[2:6]] == [
        (k * CHUNK, 10 * CHUNK) for k in (3, 5, 7, 9)
    ]
    assert done == {
        "id": 1,
        "ok": True,
        "path": audio,
        "dir": track_dir,
        "meta": meta,
        "thumb": os.path.join(track_dir, "cover.jpg"),
        "format": "webm",
        "file_size": 10 * CHUNK,
    }
    meta_path = os.path.join(track_dir, "meta.json")
    with open(meta_path, encoding="utf-8") as f:
        assert json.load(f) == meta
    assert harness.scripts[url]["meta_during_download"] is False
    cover = os.path.join(track_dir, "cover.jpg")
    assert os.path.isfile(cover)
    assert os.stat(meta_path).st_mtime_ns >= os.stat(cover).st_mtime_ns
    assert os.stat(meta_path).st_mtime_ns >= os.stat(audio).st_mtime_ns


def test_download_ydl_options_skip_manifests_and_thumbnails(harness):
    url = "https://www.youtube.com/watch?v=HHHHHHHHHHH"
    harness.scripts[url] = {"vid": "HHHHHHHHHHH", "title": "Opts"}
    _download(harness, 1, url)
    assert harness.stdout.terminal(1)["ok"] is True

    params = harness.scripts[url]["params"]
    assert params["extractor_args"] == {"youtube": {"skip": ["hls", "dash"]}}
    assert "writethumbnail" not in params
    assert set(params["outtmpl"]) == {"default"}


def test_download_ydl_options_fail_fast(harness):
    url = "https://www.youtube.com/watch?v=JJJJJJJJJJJ"
    harness.scripts[url] = {"vid": "JJJJJJJJJJJ", "title": "Fast fail"}
    _download(harness, 1, url)
    assert harness.stdout.terminal(1)["ok"] is True

    params = harness.scripts[url]["params"]
    assert params["retries"] == 1
    assert params["extractor_retries"] == 1
    assert params["socket_timeout"] == 8


class _FakeResponse:
    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False

    def read(self):
        return b"jpg"


@pytest.mark.parametrize("name", ["meta.json.tmp", "cover.jpg"])
def test_planted_symlink_is_not_followed(harness, monkeypatch, tmp_path, name):
    monkeypatch.setattr(core, "_fetch_thumbnail", _real_fetch_thumbnail)
    monkeypatch.setattr(core.urllib.request, "urlopen", lambda *a, **k: _FakeResponse())
    outside = tmp_path / "outside"
    outside.mkdir()
    victim = outside / "victim"
    victim.write_text("keep")
    track_dir = os.path.join(harness.cache, "Planted [KKKKKKKK]")
    os.makedirs(track_dir)
    os.symlink(victim, os.path.join(track_dir, name))
    url = "https://www.youtube.com/watch?v=KKKKKKKKKKK"
    harness.scripts[url] = {"vid": "KKKKKKKKKKK", "title": "Planted"}
    _download(harness, 1, url)

    done = harness.stdout.terminal(1)
    assert victim.read_text() == "keep"
    assert os.path.islink(os.path.join(track_dir, name))
    if name == "cover.jpg":
        assert done["ok"] is True
        assert done["thumb"] is None
    else:
        assert done["ok"] is False


def _thumb_fails(video_id: str, dest: str) -> bool:
    return False


def _thumb_raises(video_id: str, dest: str) -> bool:
    raise OSError("no network")


@pytest.mark.parametrize("fetch", [_thumb_fails, _thumb_raises])
def test_thumbnail_failure_is_not_fatal(harness, monkeypatch, fetch):
    monkeypatch.setattr(core, "_fetch_thumbnail", fetch)
    url = "https://www.youtube.com/watch?v=IIIIIIIIIII"
    harness.scripts[url] = {"vid": "IIIIIIIIIII", "title": "No cover"}
    _download(harness, 1, url)

    done = harness.stdout.terminal(1)
    assert done["ok"] is True
    assert done["thumb"] is None
    assert os.path.isfile(os.path.join(done["dir"], "meta.json"))


def test_cancel_during_progress(harness):
    url = "https://www.youtube.com/watch?v=BBBBBBBBBBB"
    harness.scripts[url] = {"vid": "BBBBBBBBBBB", "title": "Cancel me", "gate": threading.Event()}
    _download(harness, 1, url)
    harness.stdout.wait_for(lambda m: m.get("id") == 1 and m.get("event") == "started")

    harness.stdin.send({"id": 2, "cmd": "cancel", "target": 1})
    assert harness.stdout.terminal(2) == {"id": 2, "ok": True}
    harness.scripts[url]["gate"].set()

    done = harness.stdout.terminal(1)
    assert done["ok"] is False
    assert done["cancelled"] is True
    assert done["error"]
    lines = harness.stdout.lines
    assert lines.index({"id": 2, "ok": True}) < lines.index(done)


def test_three_concurrent_downloads_interleave(harness):
    barrier = threading.Barrier(3, timeout=TIMEOUT)
    urls = {}
    for rid, vid in ((11, "CCCCCCCCCC1"), (12, "DDDDDDDDDD2"), (13, "EEEEEEEEEE3")):
        url = f"https://www.youtube.com/watch?v={vid}"
        harness.scripts[url] = {"vid": vid, "title": f"Track {rid}", "barrier": barrier}
        urls[rid] = (url, vid)
    for rid, (url, _vid) in urls.items():
        _download(harness, rid, url)

    dones = {rid: harness.stdout.terminal(rid) for rid in urls}
    for rid, done in dones.items():
        assert done["ok"] is True, done
        assert done["meta"]["id"] == urls[rid][1]
        assert done["path"] == os.path.join(
            harness.cache, f"Track {rid} [{urls[rid][1][:8]}]", "audio.webm"
        )
    # All three were in flight together: every started precedes the first terminal.
    lines = harness.stdout.lines
    first_terminal = min(lines.index(d) for d in dones.values())
    started_at = [i for i, m in enumerate(lines) if m.get("event") == "started"]
    assert len(started_at) == 3
    assert max(started_at) < first_terminal


def test_download_error_reports_message_without_pip(harness, monkeypatch):
    calls = []
    for name in ("run", "Popen", "call", "check_call", "check_output"):
        monkeypatch.setattr(subprocess, name, lambda *a, **k: calls.append(a))
    url = "https://www.youtube.com/watch?v=FFFFFFFFFFF"
    harness.scripts[url] = {"error": "ERROR: [youtube] FFFFFFFFFFF: Video unavailable"}
    _download(harness, 3, url)

    done = harness.stdout.terminal(3)
    assert done["ok"] is False
    assert done["cancelled"] is False
    assert "Video unavailable" in done["error"]
    assert calls == []


def test_unknown_command_reports_error_with_id(harness):
    # video_info and lyrics were removed in protocol v2.
    for rid, cmd in ((4, "__bogus__"), (5, "video_info"), (6, "lyrics")):
        harness.stdin.send({"id": rid, "cmd": cmd})
        done = harness.stdout.terminal(rid)
        assert done["ok"] is False
        assert done["cancelled"] is False
        assert "Unknown command" in done["error"] and cmd in done["error"]


def test_eof_waits_for_running_job(harness):
    url = "https://www.youtube.com/watch?v=GGGGGGGGGGG"
    harness.scripts[url] = {"vid": "GGGGGGGGGGG", "title": "Slow", "gate": threading.Event()}
    _download(harness, 1, url)
    harness.stdout.wait_for(lambda m: m.get("id") == 1 and m.get("event") == "started")

    harness.stdin.close()
    harness.thread.join(0.3)
    assert harness.thread.is_alive()
    harness.scripts[url]["gate"].set()
    harness.thread.join(TIMEOUT)
    assert not harness.thread.is_alive()
    assert harness.stdout.terminal(1)["ok"] is True


@pytest.mark.live
def test_live_download(tmp_path):
    url = "https://www.youtube.com/watch?v=jNQXAC9IVRw"
    t0 = time.monotonic()
    proc = subprocess.Popen(
        [sys.executable, "-m", "yplayer.worker"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
    )
    assert json.loads(proc.stdout.readline())["protocol"] == 2
    req = {"id": 1, "cmd": "download", "url": url, "cache_dir": str(tmp_path)}
    proc.stdin.write(json.dumps(req) + "\n")
    proc.stdin.flush()

    msgs, started_at = [], None
    for line in proc.stdout:
        msg = json.loads(line)
        msgs.append(msg)
        if msg.get("event") == "started" and started_at is None:
            started_at = time.monotonic() - t0
        if "ok" in msg:
            break
    total = time.monotonic() - t0
    proc.stdin.close()
    assert proc.wait(TIMEOUT) == 0

    done = msgs[-1]
    assert done["ok"] is True, done
    assert [m.get("event") for m in msgs].index("started") < len(msgs) - 1
    names = os.listdir(done["dir"])
    assert {"audio.webm", "audio.m4a"} & set(names)
    assert "cover.jpg" in names
    assert done["thumb"] == os.path.join(done["dir"], "cover.jpg")
    assert "meta.json" in names
    print(f"\nlive download: started after {started_at:.2f}s, done after {total:.2f}s")
