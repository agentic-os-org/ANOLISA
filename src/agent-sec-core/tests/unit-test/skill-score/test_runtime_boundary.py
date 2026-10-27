"""Subprocess worker boundary and credential handling, without Docker."""

from pathlib import Path

import pytest
from agent_sec_cli.skill_score import runtime
from agent_sec_cli.skill_score.config import ScoreConfig
from agent_sec_cli.skill_score.contracts import ContractError, Probe
from agent_sec_cli.skill_score.io import read, save
from test_two_stage import baseline


def test_worker_resolves_from_raw_install_prefix(tmp_path: Path) -> None:
    prefix = tmp_path / "usr/local/lib/anolisa/sec-core/skill-score/cli"
    assert runtime.worker_python(prefix) == prefix.parent / "runtime/bin/python3.12"
    assert runtime.worker_python(Path("/opt/agent-sec/skill-score-cli")) == Path(
        "/opt/agent-sec/skill-score-runtime/bin/python3.12"
    )


def test_subprocess_protocol_has_no_key_or_audit_answers(tmp_path, monkeypatch):
    worker = tmp_path / "python3.12"
    worker.touch()
    monkeypatch.setattr(runtime, "WORKER_PYTHON", worker)
    key = "sk-" + "abcd1234" * 4
    monkeypatch.setenv("ANTHROPIC_API_KEY", key)
    monkeypatch.setenv(
        "ANTHROPIC_BASE_URL", "https://dashscope.aliyuncs.com/apps/anthropic"
    )
    monkeypatch.setenv("UNRELATED_TOKEN", "not-for-worker")
    calls = []

    class Process:
        returncode = 0

        def __init__(self, command, **kwargs):
            calls.append((command, kwargs))
            request = read(Path(command[-2]))
            assert request["operation"] == "run"
            assert "audit" not in request and "findings" not in request
            save(Path(command[-1]), {"probe_id": "baseline", "complete": True})
            kwargs["stdout"].write(key)

        def wait(self, timeout=None):
            return 0

    monkeypatch.setattr(runtime.subprocess, "Popen", Process)
    result = runtime.Runtime(ScoreConfig()).run(
        tmp_path, Probe.model_validate(baseline()), tmp_path, 10
    )
    assert result["complete"]
    command, kwargs = calls[0]
    assert key not in repr(command)
    assert kwargs["env"]["ANTHROPIC_API_KEY"] == key
    assert kwargs["env"]["ANTHROPIC_AUTH_TOKEN"] == key
    assert "UNRELATED_TOKEN" not in kwargs["env"]
    assert key not in (tmp_path / "worker.log").read_text()
    assert key not in (tmp_path / "worker-input.json").read_text()


def test_worker_failure_is_not_silently_complete(tmp_path, monkeypatch):
    worker = tmp_path / "python3.12"
    worker.touch()
    monkeypatch.setattr(runtime, "WORKER_PYTHON", worker)
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-" + "abcd1234" * 4)
    monkeypatch.setenv(
        "ANTHROPIC_BASE_URL", "https://dashscope.aliyuncs.com/apps/anthropic"
    )

    class Process:
        returncode = 1

        def __init__(self, command, **kwargs):
            save(Path(command[-1]), {"error": "required local image is missing"})

        def wait(self, timeout=None):
            return 1

    monkeypatch.setattr(runtime.subprocess, "Popen", Process)
    with pytest.raises(ContractError, match="local image"):
        runtime.Runtime(ScoreConfig())._invoke(
            {"operation": "environment"}, tmp_path, 10
        )
