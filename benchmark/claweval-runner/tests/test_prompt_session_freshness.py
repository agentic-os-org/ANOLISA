"""Successful prompt requests cannot reuse unchanged historical session files."""

import importlib.util
import json
import os
import sys
from pathlib import Path
from types import ModuleType, SimpleNamespace

import httpx
import pytest


@pytest.fixture
def prompt_session(monkeypatch: pytest.MonkeyPatch, tmp_path: Path):
    common = ModuleType("ce_runner._common")
    config = tmp_path / "config.json"
    config.write_text(
        json.dumps({"gateway": {"port": 1234, "auth": {"token": "offline"}}}),
        encoding="utf-8",
    )
    common.OPENCLAW_CONFIG = str(config)
    common._REPO_DIR = tmp_path
    common.load_task_yaml = lambda *args: {}
    warnings = []
    common.log = warnings.append
    sessions = tmp_path / "agent/sessions"
    sessions.mkdir(parents=True)
    common._agent_sessions_dir = lambda agent_id: str(sessions)
    infra = ModuleType("ce_runner.infra")
    for name in (
        "configure_tools",
        "cleanup_mock_services",
        "reset_services",
        "start_mock_services",
        "restart_gateway",
        "cleanup_config",
        "check_gateway",
    ):
        setattr(
            infra,
            name,
            lambda *args: pytest.fail("prompt helper must not set up infrastructure"),
        )
    package = ModuleType("ce_runner")
    package.__path__ = [str(Path(__file__).parents[1] / "src/ce_runner")]
    monkeypatch.setitem(sys.modules, "ce_runner", package)
    monkeypatch.setitem(sys.modules, "ce_runner._common", common)
    monkeypatch.setitem(sys.modules, "ce_runner.infra", infra)
    monkeypatch.setattr(sys, "path", list(sys.path))
    path = Path(__file__).parents[1] / "scripts/prompt_task.py"
    spec = importlib.util.spec_from_file_location("prompt_freshness_test", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    calls, actions = [], []

    def posted(*args, **kwargs):
        calls.append((args, kwargs))
        for action in actions:
            action()
        return SimpleNamespace(raise_for_status=lambda: None)

    monkeypatch.setattr(httpx, "post", posted)
    return module, sessions, actions, calls, warnings


def test_unchanged_historical_file_is_not_a_request_result(prompt_session):
    module, sessions, _, _, warnings = prompt_session
    (sessions / "old.jsonl").write_text("old\n", encoding="utf-8")
    assert module._send_prompt("new prompt", "agent", 10) == ""
    assert any("session" in warning.lower() for warning in warnings)


def test_new_session_wins_over_future_dated_old_file(prompt_session):
    module, sessions, actions, calls, _ = prompt_session
    old, new = sessions / "old.jsonl", sessions / "new.jsonl"
    old.write_text("old\n", encoding="utf-8")
    os.utime(old, ns=(4_000_000_000_000_000_000, 4_000_000_000_000_000_000))
    actions.append(lambda: new.write_text("new\n", encoding="utf-8"))
    assert module._send_prompt("new prompt", "agent", 10) == str(new)
    assert calls[0][1]["json"]["model"] == "openclaw/agent"
    assert calls[0][1]["timeout"] == 130


def test_updated_existing_session_is_selected(prompt_session):
    module, sessions, actions, _, _ = prompt_session
    path = sessions / "existing.jsonl"
    path.write_text("old\n", encoding="utf-8")
    stamp = path.stat().st_mtime_ns

    def appended():
        with path.open("a", encoding="utf-8") as stream:
            stream.write("new\n")
        os.utime(path, ns=(stamp, stamp))

    actions.append(appended)
    assert module._send_prompt("prompt", "agent", 10) == str(path)


def test_trajectory_only_update_does_not_reuse_old_session(prompt_session):
    module, sessions, actions, _, _ = prompt_session
    (sessions / "old.jsonl").write_text("old\n", encoding="utf-8")
    actions.append(
        lambda: (sessions / "new.trajectory.jsonl").write_text(
            "new\n", encoding="utf-8"
        )
    )
    assert module._send_prompt("prompt", "agent", 10) == ""


def test_jsonl_directory_is_not_a_session_file(prompt_session):
    module, sessions, actions, _, _ = prompt_session
    actions.append(lambda: (sessions / "new.jsonl").mkdir())
    assert module._send_prompt("prompt", "agent", 10) == ""


def test_empty_session_directory_remains_no_result(prompt_session):
    module, _, _, _, _ = prompt_session
    assert module._send_prompt("prompt", "agent", 10) == ""


def test_failed_http_call_remains_no_result(prompt_session, monkeypatch):
    module, sessions, _, _, _ = prompt_session
    (sessions / "old.jsonl").write_text("old\n", encoding="utf-8")

    def failed(*args, **kwargs):
        raise httpx.ConnectError("offline failure")

    monkeypatch.setattr(httpx, "post", failed)
    assert module._send_prompt("prompt", "agent", 10) == ""


def test_unreadable_directory_cannot_certify_freshness_but_request_runs(
    prompt_session, monkeypatch
):
    module, sessions, _, calls, warnings = prompt_session
    original = Path.iterdir

    def entries(path):
        if path == sessions:
            raise PermissionError("cannot observe sessions")
        return original(path)

    monkeypatch.setattr(Path, "iterdir", entries)
    assert module._send_prompt("prompt", "agent", 10) == ""
    assert len(calls) == 1
    assert any("Cannot inspect sessions" in warning for warning in warnings)


def test_unknown_before_state_cannot_make_old_file_fresh(prompt_session, monkeypatch):
    module, sessions, _, _, _ = prompt_session
    path = sessions / "old.jsonl"
    path.write_text("old\n", encoding="utf-8")
    original = Path.stat
    failures = [True]

    def inspected(file, *args, **kwargs):
        if file == path and failures:
            failures.pop()
            raise PermissionError("cannot inspect old file")
        return original(file, *args, **kwargs)

    monkeypatch.setattr(Path, "stat", inspected)
    assert module._send_prompt("prompt", "agent", 10) == ""


def test_equal_fresh_timestamps_have_deterministic_filename_order(prompt_session):
    module, sessions, actions, _, _ = prompt_session
    first, last = sessions / "a.jsonl", sessions / "z.jsonl"

    def created():
        for path in (first, last):
            path.write_text("new\n", encoding="utf-8")
            os.utime(path, ns=(2_000_000_000_000_000_000, 2_000_000_000_000_000_000))

    actions.append(created)
    assert module._send_prompt("prompt", "agent", 10) == str(last)
