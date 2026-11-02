"""Cosh native approval projection; daemon enforcement is covered by Linux E2E."""

import json
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest
from standalone_hook_test_loader import load_standalone_hook

HOOKS = Path(__file__).resolve().parents[3] / "cosh-extension" / "hooks"
bridge = load_standalone_hook(
    "sandbox_authorization_test", HOOKS / "sandbox_authorization.py"
)
guard = load_standalone_hook(
    "sandbox_guard_authorization_test", HOOKS / "sandbox-guard.py"
)


@pytest.fixture(autouse=True)
def setup(monkeypatch):
    monkeypatch.setenv(bridge.PERMISSIONS_ENV, "/config/permissions.json")
    monkeypatch.setattr(bridge.shutil, "which", lambda _: "/usr/bin/agent-sec-cli")


def test_sandbox_guard_registers_only_the_cosh_ng_lifecycle_events() -> None:
    manifest = json.loads((HOOKS.parent / "cosh-extension.json").read_text())
    events = sorted(
        event
        for event, groups in manifest["hooks"].items()
        for group in groups
        for hook in group["hooks"]
        if hook["command"].endswith("/sandbox-guard.py")
    )
    assert events == ["PostToolUse", "PreToolUse"]


@pytest.mark.parametrize("decision", ["allow", "ask", "block"])
def test_authorization_preserves_native_decision_without_executing(
    monkeypatch, decision
):
    output = {
        "decision": decision,
        "reason": "Policy explanation",
        "systemMessage": "Policy explanation\nWarning: network disabled",
        "hookSpecificOutput": {"tool_input": {"command": "handoff"}},
    }
    calls = []

    def run(args, **kwargs):
        calls.append((args, kwargs))
        return SimpleNamespace(returncode=0, stdout=json.dumps(output), stderr="")

    monkeypatch.setattr(bridge.subprocess, "run", run)
    assert (
        bridge.prepare_authorized({"session_id": "s", "cwd": "/work"}, "rm -rf cache")
        == output
    )
    assert calls[0][0][1:4] == ["sandbox", "hook", "prepare"]
    assert json.loads(calls[0][1]["input"])["tool_input"]["command"] == "rm -rf cache"


@pytest.mark.parametrize("failure", ["timeout", "daemon", "json", "empty", "missing"])
def test_authorization_failure_never_falls_back(monkeypatch, failure):
    def run(*args, **kwargs):
        if failure == "timeout":
            raise subprocess.TimeoutExpired("prepare", 55)
        return SimpleNamespace(
            returncode=1 if failure == "daemon" else 0,
            stdout="oops" if failure == "json" else json.dumps({"decision": "allow"}),
            stderr="unavailable",
        )

    monkeypatch.setattr(bridge.subprocess, "run", run)
    if failure == "missing":
        monkeypatch.setattr(bridge.shutil, "which", lambda _: None)
    if failure == "empty":
        monkeypatch.setenv(bridge.PERMISSIONS_ENV, "")
    result = bridge.prepare_authorized({}, "rm -rf cache")
    assert result["decision"] == "block"
    assert "hookSpecificOutput" not in result


def test_post_tool_receipt_does_not_run_target(monkeypatch):
    calls = []

    def run(args, **kwargs):
        calls.append(args)
        return SimpleNamespace(returncode=0, stdout="", stderr="")

    monkeypatch.setattr(bridge.subprocess, "run", run)
    monkeypatch.setenv("AGENT_SEC_DAEMON_SOCKET", "/run/a b.sock")
    payload = {
        "tool_input": {
            "command": "'/usr/bin/agent-sec-cli' '--socket' '/run/a b.sock' 'sandbox' 'hook' 'execute' 'id-1'"
        }
    }
    assert bridge.finish_authorized(payload) == {}
    assert calls == [
        [
            "/usr/bin/agent-sec-cli",
            "--socket",
            "/run/a b.sock",
            "sandbox",
            "hook",
            "finish",
            "id-1",
        ]
    ]
    assert bridge.finish_authorized({"tool_input": {"command": "echo ordinary"}}) == {}
    command = payload["tool_input"]["command"]
    for unrelated in (
        command.replace("/usr/bin/agent-sec-cli", "echo"),
        command.replace("/run/a b.sock", "/run/other.sock"),
    ):
        assert bridge.finish_authorized({"tool_input": {"command": unrelated}}) == {}
    monkeypatch.delenv(bridge.PERMISSIONS_ENV)
    assert bridge.finish_authorized(payload) == {}
    assert len(calls) == 1


def test_guard_uses_pdp_only_for_existing_sandbox_matches(monkeypatch, capsys):
    import io

    calls = []
    monkeypatch.setattr(
        guard,
        "prepare_authorized",
        lambda payload, command: calls.append(command) or {"decision": "ask"},
    )
    for command, expected in [
        ("ls", "allow"),
        ("rm -rf cache", "ask"),
        ("shutdown -h now", "block"),
    ]:
        monkeypatch.setattr(
            "sys.stdin",
            io.StringIO(
                json.dumps(
                    {
                        "session_id": "s",
                        "cwd": "/work",
                        "tool_input": {"command": command},
                    }
                )
            ),
        )
        monkeypatch.setattr(guard, "_log_sandbox_event", lambda *args, **kwargs: None)
        guard.main()
        assert json.loads(capsys.readouterr().out)["decision"] == expected
    assert calls == ["rm -rf cache"]


def test_configured_authorization_rejects_malformed_hook_input(monkeypatch, capsys):
    import io

    monkeypatch.setattr("sys.stdin", io.StringIO("invalid-json"))
    guard.main()
    assert json.loads(capsys.readouterr().out)["decision"] == "block"
