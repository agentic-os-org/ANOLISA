"""Unit tests for the Qwen Code PII checker command hook."""

import io
import json
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest
from standalone_hook_test_loader import load_standalone_hook

_ROOT = Path(__file__).resolve().parents[3]
_EXTENSION_DIR = _ROOT / "qwen-code-extension"
_HOOK_PATH = _EXTENSION_DIR / "hooks" / "pii_checker_hook.py"

pii_checker_hook = load_standalone_hook("qwen_pii_checker_hook", _HOOK_PATH)


@pytest.fixture(autouse=True)
def _clean_environment(monkeypatch):
    for name in (
        "PII_CHECKER_HOOK_ENABLED",
        "PII_CHECKER_ENABLED",
        "PII_CHECKER_MODE",
        "PII_CHECKER_INCLUDE_LOW_CONFIDENCE",
        "PII_CHECKER_TIMEOUT",
    ):
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setenv("QWEN_CODE_SESSION_ID", "env-session")


def _base(event_name, **fields):
    return {
        "hook_event_name": event_name,
        "session_id": "session-123",
        **fields,
    }


def _scan_result(verdict="pass", evidence=""):
    findings = []
    if evidence:
        findings = [
            {
                "type": "credential",
                "severity": verdict,
                "evidence_redacted": evidence,
                "raw_evidence": "raw-secret-value",
            }
        ]
    return {"verdict": verdict, "findings": findings}


def _run_main(monkeypatch, capsys, payload):
    if isinstance(payload, bytes):
        stdin = SimpleNamespace(buffer=io.BytesIO(payload))
    else:
        encoded = payload if isinstance(payload, str) else json.dumps(payload)
        stdin = io.StringIO(encoded)
    monkeypatch.setattr(pii_checker_hook.sys, "stdin", stdin)
    pii_checker_hook.main()
    captured = capsys.readouterr()
    return json.loads(captured.out), captured.err


@pytest.mark.parametrize(
    ("payload", "expected_text", "expected_source"),
    [
        (
            _base("UserPromptSubmit", prompt="contact alice@example.com"),
            "contact alice@example.com",
            "user_input",
        ),
        (
            _base(
                "PreToolUse",
                tool_input={"token": "secret-value"},
                tool_call_id="call-1",
            ),
            '{"token":"secret-value"}',
            "tool_input",
        ),
        (
            _base(
                "PostToolUse",
                tool_response={"stdout": "alice@example.com"},
                tool_call_id="call-1",
            ),
            '{"stdout":"alice@example.com"}',
            "tool_output",
        ),
        (
            _base(
                "PostToolUseFailure",
                error="token=secret-value",
                tool_use_id="tool-use-1",
            ),
            "token=secret-value",
            "tool_output",
        ),
        (
            _base("Stop", last_assistant_message="contact alice@example.com"),
            "contact alice@example.com",
            "model_output",
        ),
        (
            _base("StopFailure", last_assistant_message="partial alice@example.com"),
            "partial alice@example.com",
            "model_output",
        ),
    ],
)
def test_scans_supported_sources_via_stdin(
    monkeypatch, capsys, payload, expected_text, expected_source
):
    captured = {}

    def fake_run(args, **kwargs):
        captured["args"] = args
        captured["kwargs"] = kwargs
        return subprocess.CompletedProcess(
            args=args,
            returncode=0,
            stdout=json.dumps(_scan_result()),
            stderr="",
        )

    monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)

    output, stderr = _run_main(monkeypatch, capsys, payload)

    assert output == {}
    assert stderr == ""
    assert captured["kwargs"]["input"] == expected_text
    assert captured["kwargs"]["timeout"] == 5.0
    assert expected_text not in json.dumps(captured["args"])
    assert captured["args"][0:2] == ["agent-sec-cli", "--trace-context"]
    assert captured["args"][3:] == [
        "scan-pii",
        "--stdin",
        "--format",
        "json",
        "--redact-output",
        "--source",
        expected_source,
    ]


def test_trace_context_prefers_qwen_identifiers_and_bounds_values(monkeypatch, capsys):
    captured = {}

    def fake_run(args, **kwargs):
        captured["args"] = args
        return SimpleNamespace(
            returncode=0,
            stdout=json.dumps(_scan_result()),
            stderr="",
        )

    monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)
    payload = _base(
        "PreToolUse",
        trace_id=" trace-1 ",
        run_id="run-1",
        call_id="call-1",
        tool_input={"command": "pwd"},
        tool_call_id="preferred-tool",
        tool_use_id="fallback-tool",
    )

    output, _stderr = _run_main(monkeypatch, capsys, payload)

    assert output == {}
    assert json.loads(captured["args"][2]) == {
        "agent_name": "qwen-code",
        "trace_id": "trace-1",
        "session_id": "session-123",
        "run_id": "run-1",
        "call_id": "call-1",
        "tool_call_id": "preferred-tool",
    }

    long_payload = dict(payload, session_id="s" * 300)
    _run_main(monkeypatch, capsys, long_payload)
    assert len(json.loads(captured["args"][2])["session_id"]) == 256


def test_include_low_confidence_and_timeout_configuration(monkeypatch, capsys):
    captured = {}

    def fake_run(args, **kwargs):
        captured["args"] = args
        captured["timeout"] = kwargs["timeout"]
        return SimpleNamespace(
            returncode=0,
            stdout=json.dumps(_scan_result()),
            stderr="",
        )

    monkeypatch.setenv("PII_CHECKER_INCLUDE_LOW_CONFIDENCE", "yes")
    monkeypatch.setenv("PII_CHECKER_TIMEOUT", "99")
    monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)

    output, _stderr = _run_main(
        monkeypatch, capsys, _base("UserPromptSubmit", prompt="hello")
    )

    assert output == {}
    assert captured["args"][-1] == "--include-low-confidence"
    assert captured["timeout"] == 8.0


@pytest.mark.parametrize("value", ["bad", "0", "-1", "nan", "inf"])
def test_invalid_timeout_uses_default(monkeypatch, value):
    monkeypatch.setenv("PII_CHECKER_TIMEOUT", value)
    assert pii_checker_hook._timeout_seconds() == 5.0


@pytest.mark.parametrize("value", ["false", "0", "no", "off"])
def test_disabled_checker_skips_cli(monkeypatch, capsys, value):
    monkeypatch.setenv("PII_CHECKER_ENABLED", value)
    monkeypatch.setattr(
        pii_checker_hook.subprocess,
        "run",
        lambda *_args, **_kwargs: pytest.fail("CLI should not be called"),
    )

    output, stderr = _run_main(
        monkeypatch,
        capsys,
        _base("UserPromptSubmit", prompt="alice@example.com"),
    )

    assert output == {}
    assert stderr == ""


def test_new_disabled_switch_short_circuits_before_reading_input(monkeypatch, capsys):
    monkeypatch.setenv("PII_CHECKER_HOOK_ENABLED", "false")
    monkeypatch.setattr(
        pii_checker_hook,
        "_read_hook_input",
        lambda: pytest.fail("input should not be read"),
    )
    monkeypatch.setattr(
        pii_checker_hook.subprocess,
        "run",
        lambda *_args, **_kwargs: pytest.fail("CLI should not be called"),
    )

    pii_checker_hook.main()

    captured = capsys.readouterr()
    assert json.loads(captured.out) == {}
    assert captured.err == ""


@pytest.mark.parametrize("value", ["true", "invalid"])
def test_new_enabled_switch_overrides_legacy_disabled_value(monkeypatch, capsys, value):
    monkeypatch.setenv("PII_CHECKER_HOOK_ENABLED", value)
    monkeypatch.setenv("PII_CHECKER_ENABLED", "false")
    calls = []

    def fake_run(*_args, **_kwargs):
        calls.append(True)
        return SimpleNamespace(
            returncode=0,
            stdout=json.dumps(_scan_result()),
            stderr="",
        )

    monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)

    output, _stderr = _run_main(
        monkeypatch,
        capsys,
        _base("UserPromptSubmit", prompt="alice@example.com"),
    )

    assert output == {}
    assert calls == [True]


def test_warn_policy_warns_and_continues(monkeypatch, capsys):
    monkeypatch.setenv("PII_CHECKER_MODE", "warn")
    monkeypatch.setattr(
        pii_checker_hook.subprocess,
        "run",
        lambda args, **kwargs: SimpleNamespace(
            returncode=0,
            stdout=json.dumps(_scan_result("deny", "token=[REDACTED]")),
            stderr="",
        ),
    )

    output, _stderr = _run_main(
        monkeypatch,
        capsys,
        _base("UserPromptSubmit", prompt="raw-secret-value"),
    )

    assert set(output) == {"systemMessage"}
    message = output["systemMessage"]
    assert "1 high-risk sensitive data finding" in message
    assert "warning only" in message
    assert "token=[REDACTED]" not in message
    assert "credential" not in message
    assert "severity" not in message


def test_ask_policy_requests_pre_tool_approval(monkeypatch, capsys):
    monkeypatch.setenv("PII_CHECKER_MODE", "ask")
    monkeypatch.setattr(
        pii_checker_hook.subprocess,
        "run",
        lambda args, **kwargs: SimpleNamespace(
            returncode=0,
            stdout=json.dumps(_scan_result("deny", "token=[REDACTED]")),
            stderr="",
        ),
    )

    output, _stderr = _run_main(
        monkeypatch,
        capsys,
        _base("PreToolUse", tool_input={"token": "raw-secret-value"}),
    )

    hook_output = output["hookSpecificOutput"]
    assert hook_output["permissionDecision"] == "ask"
    assert "Confirmation is required" in hook_output["permissionDecisionReason"]


def test_ask_policy_falls_back_to_warning_without_confirmation(monkeypatch, capsys):
    monkeypatch.setenv("PII_CHECKER_MODE", "ask")
    monkeypatch.setattr(
        pii_checker_hook.subprocess,
        "run",
        lambda args, **kwargs: SimpleNamespace(
            returncode=0,
            stdout=json.dumps(_scan_result("deny", "token=[REDACTED]")),
            stderr="",
        ),
    )

    output, _stderr = _run_main(
        monkeypatch,
        capsys,
        _base("UserPromptSubmit", prompt="raw-secret-value"),
    )

    assert set(output) == {"systemMessage"}
    assert "This stage cannot confirm or block" in output["systemMessage"]
    assert "warning only" in output["systemMessage"]


@pytest.mark.parametrize(
    ("event_name", "fields", "continuation"),
    [
        (
            "PostToolUse",
            {"tool_response": {"stdout": "raw-secret-value"}},
            "raw output will enter model context",
        ),
        (
            "Stop",
            {"last_assistant_message": "raw-secret-value"},
            "response will continue",
        ),
    ],
)
def test_ask_policy_unsupported_stages_report_actual_warning_result(
    monkeypatch, capsys, event_name, fields, continuation
):
    monkeypatch.setenv("PII_CHECKER_MODE", "ask")
    monkeypatch.setattr(
        pii_checker_hook.subprocess,
        "run",
        lambda args, **kwargs: SimpleNamespace(
            returncode=0,
            stdout=json.dumps(_scan_result("deny", "token=[REDACTED]")),
            stderr="",
        ),
    )

    output, _stderr = _run_main(
        monkeypatch,
        capsys,
        _base(event_name, **fields),
    )

    assert set(output) == {"systemMessage"}
    message = output["systemMessage"]
    assert "This stage cannot confirm or block" in message
    assert "warning only" in message
    assert continuation in message
    if event_name == "PostToolUse":
        assert "tool has already run" in message
        assert "external side effects were not undone" in message


def test_observe_deny_uses_only_redacted_evidence(monkeypatch, capsys):
    def fake_run(args, **kwargs):
        return SimpleNamespace(
            returncode=0,
            stdout=json.dumps(_scan_result("deny", "password=[REDACTED]")),
            stderr="scanner leaked raw-secret-value",
        )

    monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)

    output, stderr = _run_main(
        monkeypatch,
        capsys,
        _base("PreToolUse", tool_input={"password": "raw-secret-value"}),
    )

    assert output == {}
    assert stderr == ""


def test_warn_never_blocks_in_block_mode(monkeypatch, capsys):
    monkeypatch.setenv("PII_CHECKER_MODE", "block")
    monkeypatch.setattr(
        pii_checker_hook.subprocess,
        "run",
        lambda args, **kwargs: SimpleNamespace(
            returncode=0,
            stdout=json.dumps(_scan_result("warn", "a***@example.com")),
            stderr="",
        ),
    )

    output, _stderr = _run_main(
        monkeypatch,
        capsys,
        _base("UserPromptSubmit", prompt="alice@example.com"),
    )

    assert set(output) == {"systemMessage"}
    assert "1 general-risk sensitive data finding" in output["systemMessage"]
    assert "a***@example.com" not in output["systemMessage"]


@pytest.mark.parametrize(
    ("event_name", "fields", "expected"),
    [
        (
            "UserPromptSubmit",
            {"prompt": "raw-secret-value"},
            {"decision": "block"},
        ),
        (
            "PreToolUse",
            {"tool_input": {"token": "raw-secret-value"}},
            {"permissionDecision": "deny"},
        ),
        (
            "PostToolUse",
            {"tool_response": {"stdout": "raw-secret-value"}},
            {"decision": "block"},
        ),
        (
            "Stop",
            {"last_assistant_message": "raw-secret-value", "stop_hook_active": False},
            {"decision": "block"},
        ),
    ],
)
def test_block_mode_maps_deny_to_qwen_decision(
    monkeypatch, capsys, event_name, fields, expected
):
    monkeypatch.setenv("PII_CHECKER_MODE", "block")
    monkeypatch.setattr(
        pii_checker_hook.subprocess,
        "run",
        lambda args, **kwargs: SimpleNamespace(
            returncode=0,
            stdout=json.dumps(_scan_result("deny", "token=[REDACTED]")),
            stderr="",
        ),
    )

    output, _stderr = _run_main(monkeypatch, capsys, _base(event_name, **fields))

    if event_name == "PreToolUse":
        assert output["hookSpecificOutput"]["permissionDecision"] == "deny"
        reason = output["hookSpecificOutput"]["permissionDecisionReason"]
    elif event_name == "PostToolUse":
        assert set(output) == {"continue", "stopReason", "decision", "reason"}
        assert output["continue"] is False
        assert output["decision"] == expected["decision"]
        assert output["stopReason"] == output["reason"]
        reason = output["reason"]
    else:
        assert output["decision"] == expected["decision"]
        reason = output["reason"]
    assert "1 high-risk sensitive data finding" in reason
    assert "token=[REDACTED]" not in reason
    if event_name == "UserPromptSubmit":
        assert "blocked this request" in reason
    elif event_name == "PreToolUse":
        assert "blocked this tool call" in reason
    elif event_name == "PostToolUse":
        assert "tool has already run" in reason
        assert "raw output will not enter model context" in reason
        assert "external side effects were not undone" in reason
    else:
        assert "final response was blocked" in reason
        assert "Rewrite it without sensitive data" in reason
    assert "raw-secret-value" not in json.dumps(output)


@pytest.mark.parametrize(
    ("mode", "verdict"),
    [("observe", "deny"), ("block", "warn")],
)
def test_post_tool_use_warn_or_observe_does_not_stop(
    monkeypatch, capsys, mode, verdict
):
    monkeypatch.setenv("PII_CHECKER_MODE", mode)
    monkeypatch.setattr(
        pii_checker_hook.subprocess,
        "run",
        lambda args, **kwargs: SimpleNamespace(
            returncode=0,
            stdout=json.dumps(_scan_result(verdict, "token=[REDACTED]")),
            stderr="",
        ),
    )

    output, _stderr = _run_main(
        monkeypatch,
        capsys,
        _base("PostToolUse", tool_response={"stdout": "raw-secret-value"}),
    )

    if mode == "observe":
        assert output == {}
    else:
        assert set(output) == {"systemMessage"}
        assert "continue" not in output
        assert "tool has already run" in output["systemMessage"]
        assert "raw output will enter model context" in output["systemMessage"]
        assert "external side effects were not undone" in output["systemMessage"]


@pytest.mark.parametrize("mode", ["observe", "block"])
@pytest.mark.parametrize("verdict", ["pass", "warn", "deny"])
def test_post_tool_use_failure_is_audit_only(monkeypatch, capsys, mode, verdict):
    monkeypatch.setenv("PII_CHECKER_MODE", mode)
    captured = {}
    evidence = "token=[REDACTED]" if verdict != "pass" else ""

    def fake_run(args, **kwargs):
        captured["args"] = args
        captured["input"] = kwargs["input"]
        return SimpleNamespace(
            returncode=0,
            stdout=json.dumps(_scan_result(verdict, evidence)),
            stderr="scanner leaked raw-secret-value",
        )

    monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)

    output, stderr = _run_main(
        monkeypatch,
        capsys,
        _base(
            "PostToolUseFailure",
            error="raw-secret-value",
            tool_call_id="call-failure",
        ),
    )

    assert output == {}
    assert stderr == ""
    assert captured["input"] == "raw-secret-value"
    assert captured["args"][-1] == "tool_output"
    assert "raw-secret-value" not in json.dumps(captured["args"])


@pytest.mark.parametrize("mode", ["deny", "BLOCK"])
def test_block_mode_aliases(monkeypatch, mode):
    monkeypatch.setenv("PII_CHECKER_MODE", mode)
    assert pii_checker_hook._mode() == "block"


def test_stop_hook_active_warns_without_retrying(monkeypatch, capsys):
    monkeypatch.setenv("PII_CHECKER_MODE", "block")
    monkeypatch.setattr(
        pii_checker_hook.subprocess,
        "run",
        lambda args, **kwargs: SimpleNamespace(
            returncode=0,
            stdout=json.dumps(_scan_result("deny", "a***@example.com")),
            stderr="",
        ),
    )

    output, _stderr = _run_main(
        monkeypatch,
        capsys,
        _base(
            "Stop",
            last_assistant_message="alice@example.com",
            stop_hook_active=True,
        ),
    )

    assert set(output) == {"systemMessage"}
    assert "warning-only" in output["systemMessage"]
    assert "retry loop" in output["systemMessage"]
    assert "a***@example.com" not in output["systemMessage"]


def test_stop_failure_is_audit_only(monkeypatch, capsys):
    monkeypatch.setenv("PII_CHECKER_MODE", "block")
    calls = []

    def fake_run(args, **kwargs):
        calls.append(kwargs["input"])
        return SimpleNamespace(
            returncode=0,
            stdout=json.dumps(_scan_result("deny", "a***@example.com")),
            stderr="",
        )

    monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)

    output, _stderr = _run_main(
        monkeypatch,
        capsys,
        _base("StopFailure", last_assistant_message="alice@example.com"),
    )

    assert calls == ["alice@example.com"]
    assert output == {}


@pytest.mark.parametrize(
    "scan_result",
    [
        {"verdict": "error", "findings": []},
        {"verdict": "unknown", "findings": [{}]},
        {"verdict": "deny", "findings": []},
        {"verdict": "deny", "findings": ["not-an-object"]},
        {"verdict": "deny", "findings": [{"raw_evidence": "secret"}]},
        {"verdict": "warn", "findings": "not-a-list"},
        [],
        "not-an-object",
    ],
)
def test_invalid_scan_results_fail_open(monkeypatch, capsys, scan_result):
    monkeypatch.setattr(
        pii_checker_hook.subprocess,
        "run",
        lambda args, **kwargs: SimpleNamespace(
            returncode=0,
            stdout=json.dumps(scan_result),
            stderr="secret scanner failure",
        ),
    )

    output, stderr = _run_main(
        monkeypatch,
        capsys,
        _base("UserPromptSubmit", prompt="secret"),
    )

    assert output == {}
    assert stderr == ""


@pytest.mark.parametrize(
    "result",
    [
        SimpleNamespace(returncode=1, stdout="secret", stderr="secret"),
        SimpleNamespace(returncode=0, stdout="not-json", stderr="secret"),
    ],
)
def test_cli_failures_fail_open(monkeypatch, capsys, result):
    monkeypatch.setattr(
        pii_checker_hook.subprocess, "run", lambda args, **kwargs: result
    )

    output, stderr = _run_main(
        monkeypatch,
        capsys,
        _base("UserPromptSubmit", prompt="secret"),
    )

    assert output == {}
    assert stderr == ""


@pytest.mark.parametrize(
    "exception",
    [FileNotFoundError("secret"), subprocess.TimeoutExpired(["secret"], 1)],
)
def test_cli_exceptions_fail_open(monkeypatch, capsys, exception):
    def fake_run(*_args, **_kwargs):
        raise exception

    monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)

    output, stderr = _run_main(
        monkeypatch,
        capsys,
        _base("UserPromptSubmit", prompt="secret"),
    )

    assert output == {}
    assert stderr == ""


@pytest.mark.parametrize("payload", ["not-json", "[]", "{}"])
def test_invalid_or_empty_hook_input_skips_cli(monkeypatch, capsys, payload):
    monkeypatch.setattr(
        pii_checker_hook.subprocess,
        "run",
        lambda *_args, **_kwargs: pytest.fail("CLI should not be called"),
    )

    output, stderr = _run_main(monkeypatch, capsys, payload)

    assert output == {}
    assert stderr == ""


def test_oversized_hook_input_skips_cli_without_disclosure(monkeypatch, capsys):
    secret = b"alice@example.com"
    payload = secret + b"x" * pii_checker_hook._MAX_PAYLOAD_SIZE
    monkeypatch.setattr(
        pii_checker_hook.subprocess,
        "run",
        lambda *_args, **_kwargs: pytest.fail("CLI should not be called"),
    )

    output, stderr = _run_main(monkeypatch, capsys, payload)

    assert output == {}
    assert stderr == ""


def test_unexpected_error_is_silent_and_fail_open(monkeypatch, capsys):
    def fail_target(_input_data):
        raise RuntimeError("alice@example.com")

    monkeypatch.setattr(pii_checker_hook, "_scan_target", fail_target)

    output, stderr = _run_main(
        monkeypatch,
        capsys,
        _base("UserPromptSubmit", prompt="alice@example.com"),
    )

    assert output == {}
    assert stderr == ""


def test_validated_result_keeps_only_sanitized_finding_severity():
    findings = [
        {
            "type": "credential",
            "severity": "deny",
            "evidence_redacted": "token=[REDACTED]",
            "raw_evidence": "raw-secret-value",
        },
        {"type": "email", "severity": "warn", "evidence_redacted": "a***"},
        {"type": "custom", "severity": "custom"},
    ]

    verdict, sanitized = pii_checker_hook._validated_result(
        {"verdict": "deny", "findings": findings}
    )

    assert verdict == "deny"
    assert sanitized == [
        {"severity": "deny"},
        {"severity": "warn"},
        {"severity": "custom"},
    ]
    assert pii_checker_hook._risk_summary(verdict, sanitized) == (
        "Detected 3 sensitive data findings (2 high risk, 1 general risk)"
    )
    assert pii_checker_hook._risk_summary("warn", sanitized) == (
        "Detected 3 sensitive data findings (1 high risk, 2 general risk)"
    )


def test_invalid_mode_reports_observe_fallback(monkeypatch, capsys):
    monkeypatch.setenv("PII_CHECKER_MODE", "banana")

    assert pii_checker_hook._policy() == "observe"
    message = capsys.readouterr().err
    assert "PII Checker configuration is invalid" in message
    assert "banana" not in message
    assert "observe" not in message
    assert "fallback" not in message.lower()
