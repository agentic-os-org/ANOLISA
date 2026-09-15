"""Direct Hook contracts with real Rust PII subprocesses, without Agent hosts.

Only observability record storage is isolated. Installed-layout acceptance must
load RPM plugin assets and fails if any asset is absent; there is no source fallback.
"""

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[3]
FIXTURES = ROOT / "tests/v2/fixtures"
HOSTS = ("codex", "qoder", "qwen", "cosh")
EVENTS = {
    "UserPromptSubmit": ("prompt", "user_input"),
    "PreToolUse": ("tool_input", "tool_input"),
    "PostToolUse": ("tool_response", "tool_output"),
}
SECRET = "SecretToken987"
TEXT = f"password={SECRET}"


def _asset(host):
    installed = os.environ.get("PII_HOOK_LAYOUT", "source") == "installed"
    relative = {
        "codex": "codex-plugin/hooks-plugin/hooks",
        "qoder": "qoder-plugin/hooks",
        "qwen": "qwen-code-extension/hooks",
        "cosh": "cosh-extension/hooks",
        "hermes": "hermes-plugin",
        "openclaw": "openclaw-plugin/dist",
    }[host]
    path = (Path("/opt/agent-sec") if installed else ROOT) / relative
    if installed and host == "cosh":
        path = Path("/usr/share/anolisa/extensions/agent-sec-core/hooks")
    assert path.is_dir(), f"required {host} plugin assets missing: {path}"
    return path


def _lines(path):
    return (
        [json.loads(line) for line in path.read_text().splitlines()]
        if path.exists()
        else []
    )


@pytest.fixture
def hook_environment(pii_daemon, pii_environment, tmp_path, monkeypatch):
    binary = shutil.which("agent-sec-cli")
    assert binary
    router = tmp_path / "bin"
    router.mkdir()
    entry = router / "agent-sec-cli"
    entry.write_text(
        f"#!{sys.executable}\n" + (FIXTURES / "pii_cli_router.py").read_text()
    )
    entry.chmod(0o755)
    monkeypatch.setenv("PII_TEST_RUST_CLI", binary)
    monkeypatch.setenv("PII_TEST_CALLS", str(tmp_path / "calls.jsonl"))
    monkeypatch.setenv("PII_TEST_RECORDS", str(tmp_path / "records.jsonl"))
    monkeypatch.setenv("AGENT_SEC_DAEMON_SOCKET", str(pii_daemon.socket_path))
    monkeypatch.setenv("PII_CHECKER_HOOK_ENABLED", "true")
    monkeypatch.setenv("OBSERVABILITY_HOOK_ENABLED", "true")
    monkeypatch.setenv("PATH", str(router) + os.pathsep + os.environ["PATH"])
    return pii_environment[0], tmp_path


def _run(argv, payload, extra_env=None):
    result = subprocess.run(
        argv,
        input=json.dumps(payload),
        text=True,
        capture_output=True,
        env={**os.environ, **(extra_env or {})},
        timeout=20,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert SECRET not in result.stdout + result.stderr
    return json.loads(result.stdout) if result.stdout.strip() else {}


def _payload(event, text=TEXT):
    field, _ = EVENTS[event]
    return {
        "hook_event_name": event,
        field: text,
        "tool_name": "read_file",
        "session_id": "pii-session",
        "turn_id": "pii-run",
        "run_id": "pii-run",
        "tool_use_id": "pii-tool",
        "tool_call_id": "pii-tool",
        "model": "fixture-model",
    }


def _audit(environment, source, verdict=None):
    audit, directory = environment
    events = _lines(audit / "security-events.jsonl")
    assert events, "the real Rust scanner must produce a terminal audit event"
    calls = {
        call["pid"]
        for call in _lines(directory / "calls.jsonl")
        if call["command"] == "scan-pii"
    }
    for event in events:
        assert event["category"] == "pii_scan"
        assert (
            event["pid"] in calls
        ), "audit PID must belong to the exec'd Rust subprocess"
        assert event["uid"] == os.getuid()
        assert event["details"]["request"]["source"] == source
        if verdict:
            assert event["details"]["result"]["verdict"] == verdict
    serialized = json.dumps(events)
    assert SECRET not in serialized
    assert "raw_evidence" not in serialized and "redacted_text" not in serialized
    return events


@pytest.mark.parametrize("host", HOSTS)
@pytest.mark.parametrize("event", EVENTS)
@pytest.mark.parametrize("policy", ["observe", "warn", "ask", "block"])
@pytest.mark.parametrize(
    "text,verdict",
    [("ordinary text", "pass"), ("alice@company.cn", "warn"), (TEXT, "deny")],
)
def test_standalone_pii_decisions(
    host, event, policy, text, verdict, hook_environment, monkeypatch
):
    monkeypatch.setenv("PII_CHECKER_MODE", policy)
    output = _run(
        [sys.executable, str(_asset(host) / "pii_checker_hook.py")],
        _payload(event, text),
    )
    events = _audit(hook_environment, EVENTS[event][1], verdict)
    assert len(events) == 1
    assert events[0]["session_id"] == "pii-session"
    if verdict == "pass" or policy == "observe":
        assert output in ({}, {"decision": "allow"})
    elif (
        verdict == "warn"
        or policy == "warn"
        or (
            policy == "ask"
            and (
                host == "codex"
                or event == "PostToolUse"
                or (host != "cosh" and event == "UserPromptSubmit")
            )
        )
    ):
        assert output.get("systemMessage") or (
            output.get("decision") == "allow" and output.get("reason")
        )
    elif policy == "ask":
        if host == "cosh":
            assert output["decision"] == "ask"
        else:
            assert output["hookSpecificOutput"]["permissionDecision"] == "ask"
    elif host == "qoder":
        if event == "UserPromptSubmit":
            assert output["decision"] == "deny"
        elif event == "PreToolUse":
            assert output["hookSpecificOutput"]["permissionDecision"] == "deny"
        else:
            assert output["hookSpecificOutput"]["updatedToolOutput"]
    elif host == "qwen" and event == "PreToolUse":
        assert output["hookSpecificOutput"]["permissionDecision"] == "deny"
    else:
        assert output["decision"] == "block"
        if host == "qwen" and event == "PostToolUse":
            assert output["continue"] is False and output["stopReason"]


@pytest.mark.parametrize("host", HOSTS)
@pytest.mark.parametrize("event", EVENTS)
def test_standalone_scanner_failure_preserves_fail_open(
    host, event, hook_environment, monkeypatch
):
    monkeypatch.setenv("PII_CHECKER_MODE", "block")
    monkeypatch.setenv(
        "AGENT_SEC_DAEMON_SOCKET", str(hook_environment[1] / "missing.sock")
    )
    output = _run(
        [sys.executable, str(_asset(host) / "pii_checker_hook.py")], _payload(event)
    )
    assert output in ({}, {"decision": "allow"})
    assert not _lines(hook_environment[0] / "security-events.jsonl")


def _hermes(hook, event):
    return _run(
        [sys.executable, str(FIXTURES / "hermes_pii_hook.py")],
        {"hook": hook, "event": event},
        {"PYTHONPATH": str(_asset("hermes"))},
    )


def _openclaw(hook, event):
    return _run(
        ["node", str(FIXTURES / "openclaw_pii_hook.mjs")],
        {
            "hook": hook,
            "event": event,
            "context": {
                "sessionId": "pii-session",
                "runId": "pii-run",
                "toolCallId": "pii-tool",
            },
        },
        {"PII_TEST_OPENCLAW_DIST": str(_asset("openclaw"))},
    )


@pytest.mark.parametrize("policy", ["observe", "warn", "ask", "block"])
@pytest.mark.parametrize(
    "hook,fields,source",
    [
        (
            "pre_llm_call",
            {"messages": [{"role": "user", "content": TEXT}]},
            "user_input",
        ),
        ("pre_tool_call", {"tool_name": "read", "args": TEXT}, "tool_input"),
        (
            "post_tool_call",
            {"tool_name": "read", "args": {}, "result": TEXT},
            "tool_output",
        ),
        ("post_llm_call", {"assistant_response": TEXT}, "model_output"),
    ],
)
def test_hermes_native_pii_contract(
    policy, hook, fields, source, hook_environment, monkeypatch
):
    monkeypatch.setenv("PII_CHECKER_MODE", policy)
    output = _hermes(hook, {**fields, "session_id": "pii-session"})
    if hook == "pre_tool_call" and policy == "block":
        assert output["action"] == "block" and output["message"]
    else:
        assert output is None
    assert len(_audit(hook_environment, source, "deny")) == 1


@pytest.mark.parametrize("policy", ["observe", "warn", "ask", "block"])
@pytest.mark.parametrize(
    "hook,fields,source",
    [
        ("before_dispatch", {"content": TEXT}, "user_input"),
        (
            "before_tool_call",
            {"toolName": "read", "params": {"content": TEXT}},
            "tool_input",
        ),
        ("after_tool_call", {"toolName": "read", "result": TEXT}, "tool_output"),
        ("llm_output", {"response": TEXT}, "model_output"),
    ],
)
def test_openclaw_native_pii_contract(
    policy, hook, fields, source, hook_environment, monkeypatch
):
    monkeypatch.setenv("PII_CHECKER_MODE", policy)
    output = _openclaw(hook, fields)
    decision = output["result"]
    if hook == "before_dispatch" and policy == "block":
        assert decision["handled"] is True and decision["text"]
    elif hook == "before_tool_call" and policy == "block":
        assert decision["block"] is True and decision["blockReason"]
    elif hook == "before_tool_call" and policy == "ask":
        assert decision["requireApproval"]["title"] == "PII Checker Security Review"
        assert decision["requireApproval"]["severity"] == "critical"
    else:
        assert decision is None
    if policy != "observe":
        assert any("[pii-checker]" in message for message in output["logs"])
    assert len(_audit(hook_environment, source, "deny")) == 1


@pytest.mark.parametrize("host", ["hermes", "openclaw"])
def test_native_scanner_failure_preserves_fail_open(
    host, hook_environment, monkeypatch
):
    monkeypatch.setenv("PII_CHECKER_MODE", "block")
    monkeypatch.setenv(
        "AGENT_SEC_DAEMON_SOCKET", str(hook_environment[1] / "missing.sock")
    )
    if host == "hermes":
        assert _hermes("pre_tool_call", {"tool_name": "read", "args": TEXT}) is None
    else:
        assert (
            _openclaw(
                "before_tool_call", {"toolName": "read", "params": {"content": TEXT}}
            )["result"]
            is None
        )
    assert not _lines(hook_environment[0] / "security-events.jsonl")


@pytest.mark.parametrize("host", [*HOSTS, "hermes", "openclaw"])
@pytest.mark.parametrize("available", [True, False])
def test_observability_redacts_or_drops_sensitive_fields(
    host, available, hook_environment, monkeypatch
):
    if not available:
        monkeypatch.setenv(
            "AGENT_SEC_DAEMON_SOCKET", str(hook_environment[1] / "missing.sock")
        )
    if host == "hermes":
        _hermes(
            "observability",
            {
                "hook": "before_agent_run",
                "metadata": {"sessionId": "pii-session", "runId": "pii-run"},
                "metrics": {"prompt": TEXT, "model_id": "fixture-model"},
            },
        )
    elif host == "openclaw":
        _openclaw("llm_input", {"prompt": TEXT, "model": "fixture-model"})
    else:
        _run(
            [sys.executable, str(_asset(host) / "observability_hook.py")],
            _payload("UserPromptSubmit"),
        )
    records = _lines(hook_environment[1] / "records.jsonl")
    assert len(records) == 1
    assert SECRET not in json.dumps(records)
    if available:
        assert "REDACTED" in json.dumps(records)
        _audit(hook_environment, "observability", "deny")
    else:
        assert "prompt" not in records[0]["metrics"]
        assert not _lines(hook_environment[0] / "security-events.jsonl")
