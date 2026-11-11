"""Command candidates remain requests throughout the native Cosh handoff."""

import io
import json
import shlex
import stat
import subprocess
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from standalone_hook_test_loader import load_standalone_hook

HOOKS = Path(__file__).resolve().parents[3] / "cosh-extension" / "hooks"
bridge = load_standalone_hook(
    "command_permissions_bridge", HOOKS / "sandbox_authorization.py"
)
guard = load_standalone_hook("command_permissions_guard", HOOKS / "sandbox-guard.py")
HANDOFF = shlex.join(
    [
        "/usr/bin/agent-sec-cli",
        "--socket",
        "/run/agent-sec-core/daemon.sock",
        "sandbox",
        "hook",
        "execute",
        "command-decision",
    ]
)
CANDIDATE = {
    "outcome": "candidate",
    "policy": "",
    "caveats": ["Script internals are unknown."],
}


@pytest.fixture
def dynamic(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> dict[str, str]:
    monkeypatch.setenv(bridge.SOURCE_ENV, "command")
    monkeypatch.setenv(bridge.RUNTIME_ENV, str(tmp_path / "runtime.cedar"))
    monkeypatch.delenv("AGENT_SEC_COSH_SANDBOX_PERMISSIONS", raising=False)
    monkeypatch.setenv("COSH_HOOK_EMPTY_OUTPUT_POLICY", "block")
    monkeypatch.delenv("AGENT_SEC_DAEMON_SOCKET", raising=False)
    monkeypatch.setattr(bridge.shutil, "which", lambda _: "/usr/bin/agent-sec-cli")
    return {
        "session_id": "session",
        "run_id": "run",
        "cwd": str(tmp_path),
        "task": "must not be forwarded",
    }


def response(value: object, code: int = 0) -> SimpleNamespace:
    return SimpleNamespace(
        returncode=code, stdout=json.dumps(value), stderr="test failure" if code else ""
    )


def authorized(decision: str = "allow") -> dict[str, Any]:
    result = {
        "decision": decision,
        "reason": "PDP decision",
        "systemMessage": "PDP decision",
    }
    if decision != "block":
        result["hookSpecificOutput"] = {"tool_input": {"command": HANDOFF}}
    return result


@pytest.mark.parametrize("decision", ["allow", "ask", "block"])
def test_candidate_uses_same_command_cwd_and_private_cleaned_file(
    dynamic: dict[str, str], monkeypatch: pytest.MonkeyPatch, decision: str
) -> None:
    calls, paths = [], []
    command = "printf '%s' '$secret'; python3 run.py"

    def run(args: list[str], **kwargs: Any) -> SimpleNamespace:
        calls.append((args, kwargs))
        if "propose-command" in args:
            assert args[args.index("--command") + 1] == command
            assert args[args.index("--cwd") + 1] == dynamic["cwd"]
            assert "input" not in kwargs
            assert int(args[args.index("--timeout-ms") + 1]) <= 38000
            return response(CANDIDATE)
        payload = json.loads(kwargs["input"])
        assert payload == {
            key: dynamic[key] for key in ("session_id", "run_id", "cwd")
        } | {"tool_input": {"command": command}}
        path = Path(args[args.index("--permissions") + 1])
        paths.append(path)
        assert path.read_text() == ""
        assert stat.S_IMODE(path.stat().st_mode) == 0o600
        assert "--runtime-permissions" in args
        assert kwargs["timeout"] <= bridge.PREPARE_TIMEOUT
        return response(authorized(decision))

    monkeypatch.setattr(bridge.subprocess, "run", run)
    result = bridge.prepare_authorized(dynamic, command)
    assert result["decision"] == decision
    assert (
        "Empty candidate: no business permissions requested" in result["systemMessage"]
    )
    assert CANDIDATE["caveats"][0] in result["reason"]
    assert len(calls) == 2
    assert all(not path.exists() for path in paths)


@pytest.mark.parametrize(
    "proposal,expected",
    [
        (
            {"outcome": "needs_clarification", "reasons": ["Specify the destination."]},
            "Specify the destination",
        ),
        ({"outcome": "needs_clarification", "reasons": []}, "invalid"),
        ({"outcome": "allow", "policy": ""}, "outcome"),
        ({"outcome": "candidate", "policy": None, "caveats": []}, "candidate"),
        ({"outcome": "candidate", "policy": "", "caveats": "lost"}, "explanation"),
        (
            {"outcome": "candidate", "policy": "", "caveats": ["unknown\nbehavior"]},
            "explanation",
        ),
        (CANDIDATE | {"unexpected": True}, "outcome"),
        ([], "response"),
    ],
)
def test_zero_exit_is_not_permission(
    dynamic: dict[str, str],
    monkeypatch: pytest.MonkeyPatch,
    proposal: object,
    expected: str,
) -> None:
    calls = []
    monkeypatch.setattr(
        bridge.subprocess,
        "run",
        lambda args, **kw: calls.append(args) or response(proposal),
    )
    result = bridge.prepare_authorized(dynamic, "echo test")
    assert result["decision"] == "block"
    assert expected in result["reason"]
    assert "hookSpecificOutput" not in result
    assert len(calls) == 1


@pytest.mark.parametrize("stage", ["intent", "prepare"])
@pytest.mark.parametrize("failure", ["timeout", "unavailable", "malformed", "os"])
def test_failures_never_fallback_and_clean_candidates(
    dynamic: dict[str, str], monkeypatch: pytest.MonkeyPatch, stage: str, failure: str
) -> None:
    paths, calls = [], []

    def run(args: list[str], **kwargs: Any) -> SimpleNamespace:
        calls.append(args)
        is_intent = "propose-command" in args
        if not is_intent:
            paths.append(Path(args[args.index("--permissions") + 1]))
        if is_intent and stage == "prepare":
            return response(CANDIDATE)
        if failure == "timeout":
            raise subprocess.TimeoutExpired(args, kwargs["timeout"])
        if failure == "os":
            raise OSError("service unavailable")
        if failure == "malformed":
            return SimpleNamespace(returncode=0, stdout="{", stderr="")
        return response({}, 1)

    monkeypatch.setattr(bridge.subprocess, "run", run)
    result = bridge.prepare_authorized(dynamic, "echo test")
    assert result["decision"] == "block"
    assert "hookSpecificOutput" not in result
    assert len(calls) == (1 if stage == "intent" else 2)
    assert all(not path.exists() for path in paths)
    if stage == "prepare":
        assert CANDIDATE["caveats"][0] in result["systemMessage"]


def test_generation_and_authorization_share_total_budget(
    dynamic: dict[str, str], monkeypatch: pytest.MonkeyPatch
) -> None:
    clock = [100.0]
    monkeypatch.setattr(bridge.time, "monotonic", lambda: clock[0])

    def run(args: list[str], **kwargs: Any) -> SimpleNamespace:
        if "propose-command" in args:
            clock[0] += 39
            return response(CANDIDATE)
        assert kwargs["timeout"] == 16
        assert int(args[args.index("--timeout-ms") + 1]) == 5000
        return response(authorized())

    monkeypatch.setattr(bridge.subprocess, "run", run)
    assert bridge.prepare_authorized(dynamic, "true")["decision"] == "allow"


def test_overlapping_invocations_have_independent_candidate_files(
    dynamic: dict[str, str], monkeypatch: pytest.MonkeyPatch
) -> None:
    paths = []

    def run(args: list[str], **kwargs: Any) -> SimpleNamespace:
        if "propose-command" in args:
            return response(CANDIDATE | {"policy": args[args.index("--command") + 1]})
        path = Path(args[args.index("--permissions") + 1])
        paths.append(path)
        if len(paths) == 1:
            assert bridge.prepare_authorized(dynamic, "second")["decision"] == "allow"
            assert path.read_text() == "first"
        else:
            assert path.read_text() == "second"
            assert paths[0].exists()
        return response(authorized())

    monkeypatch.setattr(bridge.subprocess, "run", run)
    assert bridge.prepare_authorized(dynamic, "first")["decision"] == "allow"
    assert paths[0] != paths[1]
    assert all(not path.exists() for path in paths)


def test_dynamic_finish_does_not_need_fixed_permission_configuration(
    dynamic: dict[str, str], monkeypatch: pytest.MonkeyPatch
) -> None:
    calls = []
    monkeypatch.setattr(
        bridge.subprocess,
        "run",
        lambda args, **kw: calls.append(args)
        or SimpleNamespace(returncode=0, stdout="", stderr=""),
    )
    assert bridge.finish_authorized({"tool_input": {"command": HANDOFF}}) == {}
    assert len(calls) == 1 and "finish" in calls[0] and "execute" not in calls[0]


def test_guard_proposes_the_original_command_including_sudo(
    dynamic: dict[str, str],
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    monkeypatch.setenv(bridge.ALL_SHELL_ENV, "true")
    calls = []
    monkeypatch.setattr(
        guard,
        "prepare_authorized",
        lambda payload, command: calls.append(command) or authorized(),
    )
    monkeypatch.setattr(
        "sys.stdin",
        io.StringIO(
            json.dumps(dynamic | {"tool_input": {"command": "sudo -n python3 run.py"}})
        ),
    )
    guard.main()
    assert json.loads(capsys.readouterr().out)["decision"] == "allow"
    assert calls == ["sudo -n python3 run.py"]


@pytest.mark.parametrize(
    "source,preset",
    [("unknown", "/runtime"), ("command", "relative"), ("", "/runtime")],
)
def test_invalid_source_or_preset_blocks_without_fixed_fallback(
    dynamic: dict[str, str], monkeypatch: pytest.MonkeyPatch, source: str, preset: str
) -> None:
    monkeypatch.setenv(bridge.SOURCE_ENV, source)
    monkeypatch.setenv(bridge.RUNTIME_ENV, preset)
    monkeypatch.setenv("AGENT_SEC_COSH_SANDBOX_PERMISSIONS", "/wide-fixed.json")
    monkeypatch.setattr(
        bridge.subprocess, "run", lambda *a, **kw: pytest.fail("must not call CLI")
    )
    assert bridge.prepare_authorized(dynamic, "echo test")["decision"] == "block"


def test_dynamic_source_cannot_bypass_host_compatibility(
    dynamic: dict[str, str], monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("COSH_HOOK_EMPTY_OUTPUT_POLICY")
    monkeypatch.setattr(
        bridge.subprocess, "run", lambda *a, **kw: pytest.fail("must not call CLI")
    )
    result = bridge.prepare_authorized(dynamic, "echo test")
    assert result["decision"] == "block"
    assert "upgrade Cosh NG" in result["reason"]


def test_dynamic_source_does_not_enable_all_shell_authorization(
    dynamic: dict[str, str],
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    monkeypatch.delenv(bridge.ALL_SHELL_ENV, raising=False)
    monkeypatch.setattr(
        guard,
        "prepare_authorized",
        lambda *a, **kw: pytest.fail("authorization must remain disabled"),
    )
    monkeypatch.setattr(
        "sys.stdin",
        io.StringIO(json.dumps(dynamic | {"tool_input": {"command": "echo test"}})),
    )
    guard.main()
    assert json.loads(capsys.readouterr().out) == {"decision": "allow"}
