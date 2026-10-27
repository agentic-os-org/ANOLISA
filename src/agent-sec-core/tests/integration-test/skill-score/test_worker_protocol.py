"""Worker file protocol and failure lifecycle without running untrusted code or Docker."""

# ruff: noqa: E402 -- the optional runtime is required for importing Harbor's worker.

import hashlib
import json
import os
import signal
import time
from pathlib import Path

import pytest

pytest.importorskip("harbor", reason="requires the locked Python 3.12 runtime")

from agent_sec_skill_score_runtime import worker
from agent_sec_skill_score_runtime.files import atomic_json


@pytest.fixture
def installed(tmp_path, monkeypatch):
    component = tmp_path / "component"
    component.mkdir()
    manifest = {
        "schema": "agent-sec-skill-score/runtime-v1",
        "protocol_version": worker.SCHEMA,
        "images": [
            {
                "role": "work",
                "id": "sha256:" + "a" * 64,
                "manifest_id": "sha256:" + "c" * 64,
            },
            {
                "role": "egress",
                "id": "sha256:" + "b" * 64,
                "manifest_id": "sha256:" + "d" * 64,
            },
        ],
    }
    raw = json.dumps(manifest).encode()
    (component / "runtime-manifest.json").write_bytes(raw)
    digest = hashlib.sha256(raw).hexdigest()
    environment = component / "environments" / digest
    environment.mkdir(parents=True)
    (environment / "runtime-manifest.json").write_bytes(raw)
    atomic_json(
        environment / "ready.json",
        {
            "schema": "agent-sec-skill-score/ready-v1",
            "manifest_sha256": digest,
            "images": manifest["images"],
        },
    )
    monkeypatch.setattr(worker.sys, "argv", [str(environment / "worker-entry.py")])
    saved = signal.getsignal(signal.SIGTERM)
    yield environment
    signal.signal(signal.SIGTERM, saved)


def request(root: Path):
    return {
        "schema_version": worker.SCHEMA,
        "request_id": "request-12345",
        "compose_project": "asc-score-12345678__env",
        "root": str(root),
        "operation": "environment",
        "timeout": 60,
        "deadline_unix_ms": (time.time() + 60) * 1000,
        "image_ids": {"work": "sha256:" + "a" * 64, "egress": "sha256:" + "b" * 64},
        "config": {},
    }


def fake_runtime(monkeypatch, calls, *, fails=False):
    class Runtime:
        def __init__(self, config, request_id, project):
            assert config.docker_image == "sha256:" + "a" * 64

        def preflight(self):
            assert os.environ["DOCKER_HOST"] == "unix:///var/run/docker.sock"
            assert "DOCKER_CONTEXT" not in os.environ
            calls.append("preflight")

        def environment(self):
            calls.append("execute")
            if fails:
                raise RuntimeError("injected execution failure")
            return {"tools": ["python"]}

    def record(root, request_id, project):
        atomic_json(
            root / "ownership.json",
            {"request_id": request_id, "compose_project": project},
        )
        calls.append("record")

    def cleanup(request_id, project):
        calls.append("cleanup")
        return "complete", []

    monkeypatch.setattr(worker, "HarborRuntime", Runtime)
    monkeypatch.setattr(worker, "record_ownership", record)
    monkeypatch.setattr(worker, "cleanup", cleanup)


@pytest.mark.parametrize("fails", [False, True])
def test_resource_ownership_precedes_execution_and_cleanup_always_runs(
    tmp_path, monkeypatch, installed, fails
):
    calls = []
    fake_runtime(monkeypatch, calls, fails=fails)
    root = tmp_path / "request"
    root.mkdir()
    response, code = worker.execute(request(root), root / "input.json")
    assert calls == ["preflight", "record", "execute", "cleanup"]
    assert response["request_id"] == "request-12345"
    assert response["schema_version"] == worker.SCHEMA
    assert response["cleanup_status"] == "complete"
    assert code == int(fails)
    assert ("error" in response) == fails


def test_manifest_mismatch_never_runs_environment(tmp_path, monkeypatch, installed):
    calls = []
    fake_runtime(monkeypatch, calls)
    root = tmp_path / "request"
    root.mkdir()
    damaged = request(root)
    damaged["image_ids"]["work"] = "sha256:" + "e" * 64
    response, code = worker.execute(damaged, root / "input.json")
    assert code == 1 and not calls
    assert "manifest" in response["error"]["message"]


def test_cleanup_pending_is_not_success(tmp_path, monkeypatch, installed):
    calls = []
    fake_runtime(monkeypatch, calls)
    monkeypatch.setattr(
        worker, "cleanup", lambda *args: ("pending", ["injected removal failure"])
    )
    root = tmp_path / "request"
    root.mkdir()
    response, code = worker.execute(request(root), root / "input.json")
    assert response["result"] == {"tools": ["python"]}
    assert response["cleanup_status"] == "pending" and code == 1


def test_candidate_preflight_uses_own_manifest_before_ready(
    tmp_path, monkeypatch, installed
):
    calls = []
    fake_runtime(monkeypatch, calls)
    (installed / "ready.json").unlink()
    (installed.parent.parent / "runtime-manifest.json").write_text("old release")
    monkeypatch.setenv("DOCKER_HOST", "tcp://unrelated.invalid:2375")
    monkeypatch.setenv("DOCKER_CONTEXT", "remote")
    root = tmp_path / "request"
    root.mkdir()
    response, code = worker.execute(request(root), root / "input.json")
    assert code == 0 and response["cleanup_status"] == "complete"
    assert calls == ["preflight", "record", "execute", "cleanup"]
