"""Protocol tests for the resident L3 headroom pipeline worker.

Spawns ``benchmark/l3-scenario/assets/worker/headroom_pipeline_worker.py``
as a subprocess with a minimal stub ``headroom`` package on PYTHONPATH (the
worker normally runs inside the headroom venv, which this test environment
does not have) and drives the documented line-delimited JSON protocol.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

WORKER = (
    Path(__file__).resolve().parents[1]
    / "benchmark"
    / "l3-scenario"
    / "assets"
    / "worker"
    / "headroom_pipeline_worker.py"
)

_CONFIG_STUB = '''\
class CacheAlignerConfig:
    def __init__(self, **kwargs):
        self.__dict__.update(kwargs)


class SmartCrusherConfig:
    def __init__(self, **kwargs):
        self.__dict__.update(kwargs)
'''

_STAGE_STUB = '''\
class _Stage:
    def __init__(self, config=None):
        self.config = config


class {name}(_Stage):
    pass
'''

_PIPELINE_STUB = '''\
from types import SimpleNamespace


class TransformPipeline:
    def __init__(self, transforms, provider):
        self._transforms = transforms
        self._provider = provider

    def apply(self, messages, model, model_limit=None):
        return SimpleNamespace(
            messages=messages,
            tokens_before=len(messages),
            tokens_after=len(messages),
            transforms_applied=[type(t).__name__ for t in self._transforms],
            timing={},
            warnings=[],
        )
'''

_ROUTER_STUB = '''\
class ContentRouter:
    def __init__(self, config=None, observer=None):
        pass
'''


@pytest.fixture()
def worker(tmp_path):
    stub_root = tmp_path / "stubs"
    pkg = stub_root / "headroom"
    transforms = pkg / "transforms"
    transforms.mkdir(parents=True)
    (pkg / "__init__.py").touch()
    (transforms / "__init__.py").touch()
    (pkg / "config.py").write_text(_CONFIG_STUB, encoding="utf-8")
    (transforms / "cache_aligner.py").write_text(
        _STAGE_STUB.format(name="CacheAligner"), encoding="utf-8"
    )
    (transforms / "smart_crusher.py").write_text(
        _STAGE_STUB.format(name="SmartCrusher"), encoding="utf-8"
    )
    (transforms / "content_router.py").write_text(_ROUTER_STUB, encoding="utf-8")
    (transforms / "pipeline.py").write_text(_PIPELINE_STUB, encoding="utf-8")
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


def _valid_request(req_id: str) -> dict:
    return {
        "messages": [{"role": "user", "content": f"msg {req_id}"}],
        "model": "benchmark-model",
        "model_limit": 200_000,
        "variant": "pure_stage",
    }


def test_handshake_ready(worker):
    handshake = _read(worker)
    assert handshake["ready"] is True
    assert "pure_stage" in handshake["variants"]


def test_unparseable_request_gets_error_response_and_worker_survives(worker):
    _read(worker)  # handshake

    worker.stdin.write("{not json\n")
    worker.stdin.flush()
    resp = _read(worker)
    assert resp.get("error")

    _send(worker, _valid_request("ok3"))
    resp = _read(worker)
    assert resp["ok"] is True


def test_non_object_request_gets_error_response_and_worker_survives(worker):
    _read(worker)  # handshake

    _send(worker, _valid_request("ok1"))
    resp = _read(worker)
    assert resp["ok"] is True

    # Valid JSON, but not an object: the protocol must answer with an error
    # line, not kill the resident worker.
    worker.stdin.write('"just a string"\n')
    worker.stdin.flush()
    resp = _read(worker)
    assert resp.get("error")
    assert resp.get("ok") is False

    # A top-level JSON array takes the same non-object path.
    worker.stdin.write("[1, 2, 3]\n")
    worker.stdin.flush()
    resp = _read(worker)
    assert resp.get("error")

    # The worker must still be alive and answering subsequent requests.
    _send(worker, _valid_request("ok2"))
    resp = _read(worker)
    assert resp["ok"] is True
