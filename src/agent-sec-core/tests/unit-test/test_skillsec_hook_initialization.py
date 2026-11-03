"""Daemon initialization ignores legacy local keys and does not reset trust."""

import subprocess
from pathlib import Path

import pytest
from standalone_hook_test_loader import load_standalone_hook

ROOT = Path(__file__).resolve().parents[2]
HOOKS = {
    "codex": "codex-plugin/hooks-plugin/hooks/skill_ledger_hook.py",
    "cosh": "cosh-extension/hooks/skill_ledger_hook.py",
    "qwen": "qwen-code-extension/hooks/skill_ledger_hook.py",
}


@pytest.mark.parametrize("host", HOOKS)
@pytest.mark.parametrize("legacy_keys", [False, True])
@pytest.mark.parametrize("exit_code", [0, 1])
def test_daemon_owns_initialization(
    host, legacy_keys, exit_code, monkeypatch, tmp_path
):
    module = load_standalone_hook(f"init_{host}", ROOT / HOOKS[host])
    monkeypatch.setenv("XDG_DATA_HOME", str(tmp_path))
    if legacy_keys:
        directory = tmp_path / "agent-sec/skill-ledger"
        directory.mkdir(parents=True)
        for name in ("key.pub", "key.enc"):
            (directory / name).write_text("obsolete key")
    calls = []

    def run(command, **kwargs):
        calls.append(command)
        return subprocess.CompletedProcess(command, exit_code, "", "private diagnostic")

    monkeypatch.setattr(module.subprocess, "run", run)
    args = ({}, "example") if host == "qwen" else ({},)
    assert module._ensure_keys(*args) is (exit_code == 0)
    assert len(calls) == 1
    assert calls[0][-3:] == ["skill-ledger", "init", "--no-baseline"]


@pytest.mark.parametrize("host", HOOKS)
def test_initialization_timeout_is_reported(host, monkeypatch, capsys):
    module = load_standalone_hook(f"init_timeout_{host}", ROOT / HOOKS[host])

    def run(command, **kwargs):
        raise subprocess.TimeoutExpired(command, 1)

    monkeypatch.setattr(module.subprocess, "run", run)
    args = ({}, "example") if host == "qwen" else ({},)
    assert module._ensure_keys(*args) is False
    assert capsys.readouterr().err
