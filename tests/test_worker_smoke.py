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
    out, _err = proc.communicate('{"cmd": "__unknown__"}\n', timeout=60)

    # Exactly one JSON line on stdout, and nothing else leaks there.
    lines = [ln for ln in out.splitlines() if ln.strip()]
    assert len(lines) == 1, f"expected one JSON line on stdout, got: {lines!r}"

    resp = json.loads(lines[0])
    assert resp["ok"] is False
    assert "__unknown__" in resp["error"]
