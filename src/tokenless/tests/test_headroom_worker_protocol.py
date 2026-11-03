"""Protocol tests for the resident L2 headroom worker.

Spawns ``benchmark/l2-module/assets/worker/headroom_worker.py`` as a subprocess
with a minimal stub ``headroom`` package on PYTHONPATH (the worker normally
runs inside the headroom venv, which this test environment does not have) and
drives the documented line-delimited JSON protocol.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

WORKER = (
    Path(__file__).resolve().parents[1]
    / "benchmark"
    / "l2-module"
    / "assets"
    / "worker"
    / "headroom_worker.py"
)

_ROUTER_STUB = '''\
from types import SimpleNamespace


class ContentRouter:
    def __init__(self, config=None, observer=None):
        pass

    def compress(self, content, context=""):
        return SimpleNamespace(
            compressed=content,
            strategy_used="stub",
            total_original_tokens=len(content),
            total_compressed_tokens=len(content),
        )
'''


@pytest.fixture()
def worker(tmp_path):
    stub_root = tmp_path / "stubs"
    transforms = stub_root / "headroom" / "transforms"
    transforms.mkdir(parents=True)
    (transforms / "content_router.py").write_text(_ROUTER_STUB, encoding="utf-8")
    (stub_root / "headroom" / "__init__.py").touch()
    (transforms / "__init__.py").touch()
    env = dict(os.environ, PYTHONPATH=str(stub_root))
    proc = subprocess.Popen(
        [sys.executable, str(WORKER)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
    )
    try:
        yield proc
    finally:
        if proc.poll() is None:
            proc.stdin.close()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=10)


def _read(proc: subprocess.Popen) -> dict:
    line = proc.stdout.readline()
    assert line, "worker produced no response line (died or hung)"
    return json.loads(line)


def _send(proc: subprocess.Popen, obj) -> None:
    proc.stdin.write(json.dumps(obj) + "\n")
    proc.stdin.flush()


def test_handshake_ready(worker):
    handshake = _read(worker)
    assert handshake["ready"] is True


def test_unparseable_request_gets_error_response_and_worker_survives(worker):
    _read(worker)  # handshake

    proc_stdin = worker.stdin
    proc_stdin.write("{not json\n")
    proc_stdin.flush()
    resp = _read(worker)
    assert resp.get("error")

    _send(worker, {"id": "ok3", "content": "x", "context": ""})
    resp = _read(worker)
    assert resp["id"] == "ok3"
    assert "error" not in resp


def test_non_object_request_gets_error_response_and_worker_survives(worker):
    _read(worker)  # handshake

    _send(worker, {"id": "ok1", "content": "hello", "context": ""})
    resp = _read(worker)
    assert resp["id"] == "ok1"
    assert "error" not in resp

    # Valid JSON, but not an object: the protocol must answer with an error
    # line, not kill the resident worker.
    worker.stdin.write("[1, 2, 3]\n")
    worker.stdin.flush()
    resp = _read(worker)
    assert resp.get("error")

    # A top-level JSON string takes the same non-object path.
    worker.stdin.write('"just a string"\n')
    worker.stdin.flush()
    resp = _read(worker)
    assert resp.get("error")

    # The worker must still be alive and answering subsequent requests.
    _send(worker, {"id": "ok2", "content": "again", "context": ""})
    resp = _read(worker)
    assert resp["id"] == "ok2"
    assert "error" not in resp
