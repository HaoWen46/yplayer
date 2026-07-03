"""Smoke + protocol tests for the Python worker.

These guard the exact class of breakage that killed HEAD in commit 4be867b:
a "clean up Python files" change left the worker importing a deleted module,
so the Rust TUI's whole download/search bridge failed at spawn. Run in CI from
a venv built out of pyproject.toml only, so undeclared dependencies fail loudly.
"""
import json
import subprocess
import sys


def _run(code: str) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, "-c", code], capture_output=True, text=True)


def test_package_imports():
    r = _run("import yplayer")
    assert r.returncode == 0, r.stderr


def test_worker_module_imports():
    # The worker imports core at module load; a dangling import breaks the bridge.
    r = _run("import yplayer.worker")
    assert r.returncode == 0, r.stderr


def test_worker_protocol_roundtrip():
    proc = subprocess.Popen(
        [sys.executable, "-m", "yplayer.worker"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    out, _err = proc.communicate('{"id": 7, "cmd": "__unknown__"}\n', timeout=60)

    # Only JSON lines on stdout, nothing else leaks there.
    lines = [json.loads(ln) for ln in out.splitlines() if ln.strip()]
    assert len(lines) == 2, f"expected ready + one response, got: {lines!r}"

    # First line is the readiness handshake.
    assert lines[0].get("event") == "ready"

    # Second line is the response, with the request id echoed back.
    resp = lines[1]
    assert resp["ok"] is False
    assert resp["id"] == 7
    assert "__unknown__" in resp["error"]
