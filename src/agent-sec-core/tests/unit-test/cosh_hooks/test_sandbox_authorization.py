"""Cosh native approval projection; daemon enforcement is covered by Linux E2E."""

import io
import json
import shlex
import shutil
import subprocess
import sys
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
failure_handler = load_standalone_hook(
    "sandbox_failure_all_shell_test", HOOKS / "sandbox-failure-handler.py"
)
paths = load_standalone_hook("sandbox_paths_test", HOOKS / "sandbox_paths.py")
HANDOFF = shlex.join(
    [
        "/usr/bin/agent-sec-cli",
        "--socket",
        "/run/agent-sec-core/daemon.sock",
        "sandbox",
        "hook",
        "execute",
        "id-1",
    ]
)


@pytest.fixture(autouse=True)
def setup(monkeypatch):
    monkeypatch.setitem(sys.modules, "sandbox_paths", paths)
    monkeypatch.setenv("COSH_HOOK_EMPTY_OUTPUT_POLICY", "block")
    monkeypatch.delenv(bridge.ALL_SHELL_ENV, raising=False)
    monkeypatch.delenv("AGENT_SEC_DAEMON_SOCKET", raising=False)
    monkeypatch.setattr(bridge.shutil, "which", lambda _: "/usr/bin/agent-sec-cli")


@pytest.mark.parametrize("policy", [None, "allow", "", "unknown"])
def test_authorization_rejects_hosts_without_output_enforcement(
    monkeypatch: pytest.MonkeyPatch, policy: str | None
) -> None:
    monkeypatch.delenv("COSH_HOOK_EMPTY_OUTPUT_POLICY")
    if policy is not None:
        monkeypatch.setenv("COSH_HOOK_EMPTY_OUTPUT_POLICY", policy)
    calls = []
    monkeypatch.setattr(
        bridge.subprocess, "run", lambda *args, **kwargs: calls.append(args)
    )
    result = bridge.prepare_authorized({}, "python3 skill.py")
    assert result["decision"] == "block"
    assert "upgrade Cosh NG" in result["systemMessage"]
    assert calls == []


@pytest.mark.parametrize("enabled", ["0", "1"])
def test_missing_path_module_only_blocks_enabled_mode(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, enabled: str
) -> None:
    hooks = tmp_path / "hooks"
    shutil.copytree(HOOKS, hooks)
    (hooks / "sandbox_paths.py").unlink()
    shutil.rmtree(hooks / "__pycache__", ignore_errors=True)
    monkeypatch.setenv(bridge.ALL_SHELL_ENV, enabled)
    result = subprocess.run(
        [sys.executable, str(hooks / "sandbox-guard.py")],
        input=json.dumps(
            {"tool_name": "shell", "tool_input": {"command": "python3 skill.py"}}
        ),
        capture_output=True,
        text=True,
        check=True,
    )
    output = json.loads(result.stdout)
    assert output["decision"] == ("block" if enabled == "1" else "allow")
    if enabled == "1":
        assert "sandbox_paths" in output["systemMessage"]
    assert "hookSpecificOutput" not in output


@pytest.mark.parametrize("tool_input", [None, [], {"command": 123}, {"command": "'"}])
def test_finish_reports_invalid_input_without_running_a_process(
    monkeypatch: pytest.MonkeyPatch, tool_input: object
) -> None:
    calls = []
    monkeypatch.setattr(
        bridge.subprocess, "run", lambda *args, **kwargs: calls.append(args)
    )
    result = bridge.finish_authorized({"tool_input": tool_input})
    assert "Sandbox receipt failed; do not rerun" in result["systemMessage"]
    assert calls == []


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
    for groups in manifest["hooks"].values():
        for group in groups:
            if any(
                hook["command"].endswith("/sandbox-guard.py") for hook in group["hooks"]
            ):
                assert group["matcher"] == "^(run_shell_command|shell)$"


@pytest.mark.parametrize("decision", ["allow", "ask", "block"])
def test_authorization_preserves_native_decision_without_executing(
    monkeypatch, decision
):
    output = {
        "decision": decision,
        "reason": "Policy explanation",
        "systemMessage": "Policy explanation\nWarning: network disabled",
    }
    if decision != "block":
        output["hookSpecificOutput"] = {"tool_input": {"command": HANDOFF}}
    calls = []

    def run(args, **kwargs):
        calls.append((args, kwargs))
        return SimpleNamespace(returncode=0, stdout=json.dumps(output), stderr="")

    monkeypatch.setattr(bridge.subprocess, "run", run)
    assert (
        bridge.prepare_authorized({"session_id": "s", "cwd": "/work"}, "rm -rf cache")
        == output
    )
    assert calls[0][0][1:] == [
        "sandbox",
        "hook",
        "prepare",
        "--permissions",
        paths.DEFAULT_PERMISSIONS_TEMPLATE,
    ]
    assert json.loads(calls[0][1]["input"])["tool_input"]["command"] == "rm -rf cache"
    assert len(calls) == 1


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
    assert bridge.finish_authorized(payload) == {}
    assert len(calls) == 2


def test_guard_preserves_legacy_routing_when_disabled(monkeypatch, capsys):
    calls = []
    monkeypatch.setattr(
        guard,
        "prepare_authorized",
        lambda payload, command: calls.append(command) or {"decision": "ask"},
    )
    monkeypatch.setattr(
        guard, "build_sandbox_command", lambda *args, **kwargs: "legacy sandbox"
    )
    for command, expected in [
        ("ls", "allow"),
        ("rm -rf cache", "allow"),
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
    assert calls == []


def test_configured_authorization_rejects_malformed_hook_input(monkeypatch, capsys):
    monkeypatch.setenv(bridge.ALL_SHELL_ENV, "1")
    monkeypatch.setattr("sys.stdin", io.StringIO("invalid-json"))
    guard.main()
    assert json.loads(capsys.readouterr().out)["decision"] == "block"


@pytest.mark.parametrize(
    "raw,decision", [("invalid-json", "allow"), ("[]", "block"), ("null", "block")]
)
def test_default_mode_keeps_malformed_input_from_becoming_an_allow(
    monkeypatch, capsys, raw, decision
):
    monkeypatch.setattr("sys.stdin", io.StringIO(raw))
    guard.main()
    assert json.loads(capsys.readouterr().out)["decision"] == decision


@pytest.mark.parametrize("tool", ["shell", "run_shell_command"])
@pytest.mark.parametrize(
    "command",
    [
        "python3 skill.py",
        "ls",
        "rm -rf cache",
        "curl example.com",
        "sudo python3 skill.py",
        "sudo -u root python3 skill.py",
    ],
)
def test_all_shell_routes_every_command_through_prepare(
    monkeypatch, capsys, tool, command
):
    monkeypatch.setenv(bridge.ALL_SHELL_ENV, "true")
    calls = []
    payload = {
        "tool_name": tool,
        "tool_input": {"command": command},
        "cwd": "/work",
        "session_id": "s",
    }
    monkeypatch.setattr("sys.stdin", io.StringIO(json.dumps(payload)))
    monkeypatch.setattr(
        guard,
        "prepare_authorized",
        lambda data, cmd: calls.append((data, cmd)) or {"decision": "ask"},
    )
    guard.main()
    assert json.loads(capsys.readouterr().out) == {"decision": "ask"}
    assert calls == [(payload, command)]


@pytest.mark.parametrize(
    "flag", [None, "false", "0", "invalid", "1", "TRUE", " true ", ""]
)
def test_all_shell_flag_is_explicit_and_invalid_values_block(monkeypatch, capsys, flag):
    if flag is not None:
        monkeypatch.setenv(bridge.ALL_SHELL_ENV, flag)
    monkeypatch.setattr(
        "sys.stdin", io.StringIO('{"tool_input":{"command":"python3 skill.py"}}')
    )
    monkeypatch.setattr(guard, "prepare_authorized", lambda *_: {"decision": "ask"})
    guard.main()
    result = json.loads(capsys.readouterr().out)
    expected = (
        "block"
        if flag in ("invalid", "")
        else "ask" if flag in ("1", "TRUE", " true ") else "allow"
    )
    assert result["decision"] == expected
    if expected == "block":
        assert bridge.ALL_SHELL_ENV in result["reason"]


def test_all_shell_keeps_explicit_blocks_without_authorizing(monkeypatch, capsys):
    monkeypatch.setenv(bridge.ALL_SHELL_ENV, "true")
    monkeypatch.setattr(
        "sys.stdin", io.StringIO('{"tool_input":{"command":"shutdown -h now"}}')
    )
    monkeypatch.setattr(
        guard,
        "prepare_authorized",
        lambda *_: pytest.fail("explicit block reached PDP"),
    )
    monkeypatch.setattr(guard, "_log_sandbox_event", lambda *args, **kwargs: None)
    guard.main()
    assert json.loads(capsys.readouterr().out)["decision"] == "block"


@pytest.mark.parametrize(
    "raw",
    [
        "oops",
        "[]",
        "null",
        "{}",
        '{"tool_input":null}',
        '{"tool_input":{"command":42}}',
        '{"tool_input":{"command":" "}}',
    ],
)
def test_all_shell_rejects_malformed_input_with_reason(monkeypatch, capsys, raw):
    monkeypatch.setenv(bridge.ALL_SHELL_ENV, "true")
    monkeypatch.setattr("sys.stdin", io.StringIO(raw))
    guard.main()
    result = json.loads(capsys.readouterr().out)
    assert result["decision"] == "block"
    assert result["reason"] and result["systemMessage"]


@pytest.mark.parametrize(
    "specific",
    [
        None,
        [],
        "bad",
        {},
        {"tool_input": None},
        {"tool_input": []},
        {"tool_input": {"command": "python3 skill.py"}},
        {"tool_input": {"command": HANDOFF + "; echo bypass"}},
        {"tool_input": {"command": HANDOFF.replace("execute", "run")}},
        {"tool_input": {"command": HANDOFF.replace("id-1", "''")}},
    ],
)
def test_malformed_execution_handoff_blocks(monkeypatch, specific):
    output = {
        "decision": "allow",
        "reason": "allowed",
        "systemMessage": "allowed",
        "hookSpecificOutput": specific,
    }
    monkeypatch.setattr(
        bridge.subprocess,
        "run",
        lambda *args, **kwargs: SimpleNamespace(
            returncode=0, stdout=json.dumps(output), stderr=""
        ),
    )
    result = bridge.prepare_authorized({}, "python3 skill.py")
    assert result["decision"] == "block"
    assert "hookSpecificOutput" not in result


@pytest.mark.parametrize(
    "output",
    [
        [],
        None,
        {"decision": []},
        {"decision": "allow"},
        {"decision": "block", "reason": None},
        {"decision": "ask", "reason": "", "systemMessage": "ask"},
    ],
)
def test_invalid_authorization_response_has_visible_error(monkeypatch, output):
    monkeypatch.setattr(
        bridge.subprocess,
        "run",
        lambda *args, **kwargs: SimpleNamespace(
            returncode=0, stdout=json.dumps(output), stderr=""
        ),
    )
    result = bridge.prepare_authorized({}, "python3 skill.py")
    assert result["decision"] == "block"
    assert "Sandbox authorization blocked execution" in result["systemMessage"]


def test_handoff_is_requoted_before_shell_execution(monkeypatch):
    output = {
        "decision": "allow",
        "reason": "allowed",
        "systemMessage": "allowed",
        "hookSpecificOutput": {
            "tool_input": {"command": HANDOFF.replace("id-1", '"$UNTRUSTED"')}
        },
    }
    monkeypatch.setattr(
        bridge.subprocess,
        "run",
        lambda *args, **kwargs: SimpleNamespace(
            returncode=0, stdout=json.dumps(output), stderr=""
        ),
    )
    result = bridge.prepare_authorized({}, "python3 skill.py")
    assert result["hookSpecificOutput"]["tool_input"]["command"].endswith(
        "'$UNTRUSTED'"
    )


@pytest.mark.parametrize("failure", ["missing", "timeout", "daemon", "response"])
def test_receipt_failure_reports_without_retry(monkeypatch, failure):
    calls = []

    def run(args, **kwargs):
        calls.append(args)
        if failure == "timeout":
            raise subprocess.TimeoutExpired(args, bridge.FINISH_TIMEOUT)
        return SimpleNamespace(
            returncode=int(failure == "daemon"),
            stdout="bad" if failure == "response" else "",
            stderr="daemon unavailable",
        )

    monkeypatch.setattr(bridge.subprocess, "run", run)
    if failure == "missing":
        monkeypatch.setattr(bridge.shutil, "which", lambda _: None)
    result = bridge.finish_authorized({"tool_input": {"command": HANDOFF}})
    assert "do not rerun" in result["systemMessage"]
    assert all(args[4:6] == ["hook", "finish"] for args in calls)
    assert len(calls) == int(failure != "missing")


@pytest.mark.parametrize("enabled", [True, False])
def test_all_shell_suppresses_bypass_without_changing_default(
    monkeypatch, capsys, enabled
):
    monkeypatch.setenv(bridge.ALL_SHELL_ENV, str(enabled).lower())
    monkeypatch.setattr(
        "sys.stdin",
        io.StringIO(
            json.dumps(
                {
                    "tool_input": {
                        "command": "linux-sandbox -- bash -c 'echo duplicate'"
                    },
                    "error": "permission denied",
                }
            )
        ),
    )
    failure_handler.main()
    output = json.loads(capsys.readouterr().out)
    if enabled:
        assert output == {}
    else:
        assert (
            output["hookSpecificOutput"]["sandbox_bypass_request"]["original_command"]
            == "echo duplicate"
        )


def test_authorization_deadlines_fit_packaged_hooks():
    manifest = json.loads((HOOKS.parent / "cosh-extension.json").read_text())
    hooks = {
        hook["name"]: hook
        for groups in manifest["hooks"].values()
        for group in groups
        for hook in group["hooks"]
    }
    assert bridge.PREPARE_TIMEOUT * 1000 + 5000 <= hooks["sandbox-guard"]["timeout"]
    assert bridge.FINISH_TIMEOUT * 1000 + 5000 <= hooks["sandbox-receipt"]["timeout"]
    assert not hooks["sandbox-guard"].get("fail_open", False)
    assert hooks["sandbox-guard"]["require_output_when_env"] == bridge.ALL_SHELL_ENV
    assert all(
        "require_output_when_env" not in hook
        for name, hook in hooks.items()
        if name != "sandbox-guard"
    )


def test_removed_permission_variable_cannot_override_template(monkeypatch):
    monkeypatch.setenv(
        "AGENT_SEC_COSH_SANDBOX_PERMISSIONS", "/attacker/permissions.json"
    )
    calls = []
    monkeypatch.setattr(
        bridge.subprocess,
        "run",
        lambda args, **kwargs: calls.append(args)
        or SimpleNamespace(returncode=1, stdout="", stderr="missing template"),
    )
    result = bridge.prepare_authorized({}, "sudo python3 skill.py")
    assert calls[0][-1] == paths.DEFAULT_PERMISSIONS_TEMPLATE
    assert result["decision"] == "block"


def test_finish_ignores_invalid_switch_after_execution(monkeypatch, capsys):
    monkeypatch.setenv(bridge.ALL_SHELL_ENV, "invalid")
    monkeypatch.setattr(
        "sys.stdin",
        io.StringIO(
            json.dumps(
                {"hook_event_name": "PostToolUse", "tool_input": {"command": HANDOFF}}
            )
        ),
    )
    calls = []
    monkeypatch.setattr(
        guard, "finish_authorized", lambda data: calls.append(data) or {}
    )
    guard.main()
    assert json.loads(capsys.readouterr().out) == {}
    assert len(calls) == 1
