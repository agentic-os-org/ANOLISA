"""Unit tests for the Qoder PII checker hook."""

import json
import os
import stat
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest
from standalone_hook_test_loader import load_standalone_hook

_PLUGIN_DIR = Path(__file__).resolve().parents[3] / "qoder-plugin"
_HOOK_SCRIPT = _PLUGIN_DIR / "hooks" / "pii_checker_hook.py"

_MOCK_CLI_SCRIPT = f"#!{sys.executable}\n" + textwrap.dedent("""\
    import json
    import os
    import sys

    stdin_text = sys.stdin.read()
    capture_path = os.environ.get("_MOCK_CLI_CAPTURE")
    if capture_path:
        with open(capture_path, "w", encoding="utf-8") as handle:
            json.dump({"argv": sys.argv[1:], "stdin": stdin_text}, handle)

    output = os.environ.get("_MOCK_CLI_OUTPUT", "")
    if output:
        print(output)
    sys.exit(int(os.environ.get("_MOCK_CLI_RC", "0")))
    """)

_PII_WARN_RESULT = json.dumps(
    {
        "verdict": "warn",
        "findings": [
            {
                "type": "email",
                "severity": "warn",
                "evidence_redacted": "a***@example.com",
            }
        ],
    }
)

_PII_DENY_RESULT = json.dumps(
    {
        "verdict": "deny",
        "findings": [
            {
                "type": "credential",
                "severity": "deny",
                "evidence_redacted": "api_key=[REDACTED]",
            }
        ],
    }
)


@pytest.fixture()
def mock_cli(tmp_path: Path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    cli = bin_dir / "agent-sec-cli"
    cli.write_text(_MOCK_CLI_SCRIPT)
    cli.chmod(cli.stat().st_mode | stat.S_IEXEC)
    capture = tmp_path / "capture.json"

    def make_env(
        output: str = "",
        *,
        rc: int = 0,
        extra: dict[str, str] | None = None,
    ) -> tuple[dict[str, str], Path]:
        env = {
            "PATH": str(bin_dir) + os.pathsep + os.environ.get("PATH", ""),
            "PYTHONPATH": str(_PLUGIN_DIR / "hooks"),
            "_MOCK_CLI_OUTPUT": output,
            "_MOCK_CLI_RC": str(rc),
            "_MOCK_CLI_CAPTURE": str(capture),
        }
        if extra:
            env.update(extra)
        return env, capture

    return make_env


def _run_hook(
    input_data: object, env: dict[str, str]
) -> subprocess.CompletedProcess[str]:
    stdin_text = (
        json.dumps(input_data) if isinstance(input_data, dict) else str(input_data)
    )
    return subprocess.run(
        [sys.executable, str(_HOOK_SCRIPT)],
        capture_output=True,
        check=False,
        env=env,
        input=stdin_text,
        text=True,
        timeout=15,
    )


def _stdout_json(proc: subprocess.CompletedProcess[str]) -> dict[str, object]:
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout.strip()
    return json.loads(proc.stdout)


def _captured_call(path: Path) -> dict[str, object]:
    return json.loads(path.read_text())


def test_invalid_json_fails_open(mock_cli) -> None:
    env, _capture = mock_cli(
        output=_PII_DENY_RESULT, extra={"PII_CHECKER_MODE": "deny"}
    )

    proc = _run_hook("{not json", env)

    assert proc.returncode == 0
    assert proc.stdout == ""


def test_environment_disabled_short_circuits_before_input_and_cli(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    monkeypatch.setenv("PII_CHECKER_HOOK_ENABLED", "false")
    disabled_hook = load_standalone_hook(
        "qoder_pii_checker_disabled_hook",
        _HOOK_SCRIPT,
    )
    monkeypatch.setattr(
        disabled_hook,
        "load_hook_input",
        lambda: pytest.fail("input should not be read"),
    )
    monkeypatch.setattr(
        disabled_hook.subprocess,
        "run",
        lambda *_args, **_kwargs: pytest.fail("CLI should not be called"),
    )

    disabled_hook.main()

    captured = capsys.readouterr()
    assert captured.out == ""
    assert captured.err == ""


def test_user_prompt_observe_scans_and_allows_silently(mock_cli) -> None:
    env, capture = mock_cli(output=_PII_DENY_RESULT)

    proc = _run_hook(
        {
            "hook_event_name": "UserPromptSubmit",
            "prompt": "phone 13800138000",
            "session_id": "sess-1",
        },
        env,
    )

    assert proc.returncode == 0
    assert proc.stdout == ""
    captured = _captured_call(capture)
    assert "--source" in captured["argv"]
    assert captured["argv"][captured["argv"].index("--source") + 1] == "user_input"
    assert captured["stdin"] == "phone 13800138000"


def test_warn_policy_warns_and_continues(mock_cli) -> None:
    env, _capture = mock_cli(
        output=_PII_DENY_RESULT,
        extra={"PII_CHECKER_MODE": "warn"},
    )

    output = _stdout_json(
        _run_hook(
            {
                "hook_event_name": "UserPromptSubmit",
                "prompt": "api key sk-live-secret",
            },
            env,
        )
    )

    assert output["decision"] == "allow"
    message = output["systemMessage"]
    assert "1 high-risk sensitive data finding" in message
    assert "warning only" in message
    assert "api_key=[REDACTED]" not in message
    assert "credential" not in message
    assert "severity" not in message


def test_ask_policy_requests_pre_tool_approval(mock_cli) -> None:
    env, _capture = mock_cli(
        output=_PII_DENY_RESULT,
        extra={"PII_CHECKER_MODE": "ask"},
    )

    output = _stdout_json(
        _run_hook(
            {
                "hook_event_name": "PreToolUse",
                "tool_input": {"token": "sk-live-secret"},
            },
            env,
        )
    )

    hook_output = output["hookSpecificOutput"]
    assert hook_output["permissionDecision"] == "ask"
    assert "Confirmation is required" in hook_output["permissionDecisionReason"]


def test_ask_policy_falls_back_to_warning_without_confirmation(mock_cli) -> None:
    env, _capture = mock_cli(
        output=_PII_DENY_RESULT,
        extra={"PII_CHECKER_MODE": "ask"},
    )

    output = _stdout_json(
        _run_hook(
            {
                "hook_event_name": "UserPromptSubmit",
                "prompt": "api key sk-live-secret",
            },
            env,
        )
    )

    assert output["decision"] == "allow"
    assert "This stage cannot confirm or block" in output["systemMessage"]
    assert "warning only" in output["systemMessage"]


def test_ask_policy_post_tool_warning_reports_actual_result(mock_cli) -> None:
    env, _capture = mock_cli(
        output=_PII_DENY_RESULT,
        extra={"PII_CHECKER_MODE": "ask"},
    )

    output = _stdout_json(
        _run_hook(
            {
                "hook_event_name": "PostToolUse",
                "tool_response": {"content": "password=secret"},
            },
            env,
        )
    )

    assert output["decision"] == "allow"
    message = output["systemMessage"]
    assert "tool has already run" in message
    assert "This stage cannot confirm or block" in message
    assert "raw output will enter model context" in message
    assert "external side effects were not undone" in message


def test_hook_trace_context_contains_only_host_correlation_ids(mock_cli) -> None:
    env, capture = mock_cli(output=_PII_WARN_RESULT)

    first = _run_hook(
        {
            "hook_event_name": "UserPromptSubmit",
            "prompt": "hello",
            "session_id": "sess-1",
        },
        env,
    )
    assert first.returncode == 0
    first_call = _captured_call(capture)
    first_index = first_call["argv"].index("--trace-context") + 1
    first_context = json.loads(first_call["argv"][first_index])

    second = _run_hook(
        {
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": "pwd"},
            "tool_use_id": "tool-1",
            "session_id": "sess-1",
        },
        env,
    )
    assert second.returncode == 0
    second_call = _captured_call(capture)
    second_index = second_call["argv"].index("--trace-context") + 1
    second_context = json.loads(second_call["argv"][second_index])

    assert first_context == {
        "agent_name": "qoder",
        "session_id": "sess-1",
    }
    assert second_context == {
        "agent_name": "qoder",
        "session_id": "sess-1",
        "tool_call_id": "tool-1",
    }


def test_user_prompt_deny_blocks_without_raw_pii(mock_cli) -> None:
    env, _capture = mock_cli(
        output=_PII_DENY_RESULT,
        extra={"PII_CHECKER_MODE": "deny"},
    )

    proc = _run_hook(
        {
            "hook_event_name": "UserPromptSubmit",
            "prompt": "api key sk-live-secret",
        },
        env,
    )

    output = _stdout_json(proc)
    assert output["decision"] == "deny"
    assert "1 high-risk sensitive data finding" in output["reason"]
    assert "blocked this request" in output["reason"]
    assert "api_key=[REDACTED]" not in output["reason"]
    assert "sk-live-secret" not in proc.stdout
    assert "sk-live-secret" not in proc.stderr


def test_warn_in_deny_mode_allows_with_system_message(mock_cli) -> None:
    env, _capture = mock_cli(
        output=_PII_WARN_RESULT,
        extra={"PII_CHECKER_MODE": "deny"},
    )

    proc = _run_hook(
        {
            "hook_event_name": "UserPromptSubmit",
            "prompt": "email alice@example.com",
        },
        env,
    )

    output = _stdout_json(proc)
    assert output["decision"] == "allow"
    assert "systemMessage" in output
    assert "1 general-risk sensitive data finding" in output["systemMessage"]
    assert "a***@example.com" not in output["systemMessage"]
    assert "alice@example.com" not in proc.stdout


def test_pre_tool_use_deny_uses_permission_decision(mock_cli) -> None:
    env, capture = mock_cli(
        output=_PII_DENY_RESULT,
        extra={"PII_CHECKER_MODE": "deny"},
    )

    proc = _run_hook(
        {
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": "curl -H 'Authorization: Bearer secret-token'"},
            "tool_use_id": "tool-1",
        },
        env,
    )

    output = _stdout_json(proc)
    hook_output = output["hookSpecificOutput"]
    assert hook_output["hookEventName"] == "PreToolUse"
    assert hook_output["permissionDecision"] == "deny"
    reason = hook_output["permissionDecisionReason"]
    assert "blocked this tool call" in reason
    assert "api_key=[REDACTED]" not in reason
    captured = _captured_call(capture)
    assert captured["argv"][captured["argv"].index("--source") + 1] == "tool_input"


def test_pre_tool_use_accepts_json_string_input(mock_cli) -> None:
    env, capture = mock_cli(output=_PII_WARN_RESULT)

    proc = _run_hook(
        {
            "hook_event_name": "PreToolUse",
            "tool_input": '{"command":"echo alice@example.com"}',
        },
        env,
    )

    assert proc.returncode == 0
    assert proc.stdout == ""
    captured = _captured_call(capture)
    assert "alice@example.com" in captured["stdin"]


def test_post_tool_use_deny_replaces_tool_output(mock_cli) -> None:
    env, capture = mock_cli(
        output=_PII_DENY_RESULT,
        extra={"PII_CHECKER_MODE": "deny"},
    )

    proc = _run_hook(
        {
            "hook_event_name": "PostToolUse",
            "tool_name": "Read",
            "tool_response": {"content": "password=secret"},
        },
        env,
    )

    output = _stdout_json(proc)
    hook_output = output["hookSpecificOutput"]
    assert hook_output["hookEventName"] == "PostToolUse"
    assert "updatedToolOutput" in hook_output
    replacement = hook_output["updatedToolOutput"]
    assert "tool has already run" in replacement
    assert "raw output will not enter model context" in replacement
    assert "external side effects were not undone" in replacement
    assert "api_key=[REDACTED]" not in replacement
    assert "password=secret" not in proc.stdout
    captured = _captured_call(capture)
    assert captured["argv"][captured["argv"].index("--source") + 1] == "tool_output"


def test_post_tool_use_warning_states_execution_and_context_result(mock_cli) -> None:
    env, _capture = mock_cli(
        output=_PII_WARN_RESULT,
        extra={"PII_CHECKER_MODE": "warn"},
    )

    output = _stdout_json(
        _run_hook(
            {
                "hook_event_name": "PostToolUse",
                "tool_response": {"content": "alice@example.com"},
            },
            env,
        )
    )

    message = output["systemMessage"]
    assert "tool has already run" in message
    assert "raw output will enter model context" in message
    assert "external side effects were not undone" in message
    assert "a***@example.com" not in message


def test_include_low_confidence_flag_is_forwarded(mock_cli) -> None:
    env, capture = mock_cli(
        output=_PII_WARN_RESULT,
        extra={"PII_CHECKER_INCLUDE_LOW_CONFIDENCE": "true"},
    )

    proc = _run_hook(
        {
            "hook_event_name": "UserPromptSubmit",
            "prompt": "hello",
        },
        env,
    )

    assert proc.returncode == 0
    assert "--include-low-confidence" in _captured_call(capture)["argv"]


def test_cli_failure_fails_open(mock_cli) -> None:
    env, _capture = mock_cli(
        output="",
        rc=1,
        extra={"PII_CHECKER_MODE": "deny"},
    )

    proc = _run_hook(
        {
            "hook_event_name": "UserPromptSubmit",
            "prompt": "api key sk-live-secret",
        },
        env,
    )

    assert proc.returncode == 0
    assert proc.stdout == ""
    assert "sk-live-secret" not in proc.stderr


def test_invalid_mode_reports_observe_fallback(mock_cli) -> None:
    env, _capture = mock_cli(
        output=_PII_WARN_RESULT,
        extra={"PII_CHECKER_MODE": "banana"},
    )

    proc = _run_hook(
        {
            "hook_event_name": "UserPromptSubmit",
            "prompt": "alice@example.com",
        },
        env,
    )
    output = _stdout_json(proc)

    assert output["decision"] == "allow"
    assert "PII Checker configuration is invalid" in output["systemMessage"]
    assert "banana" not in output["systemMessage"]
    assert "fallback" not in output["systemMessage"].lower()


def test_risk_summary_uses_finding_severity_and_verdict_fallback() -> None:
    hook = load_standalone_hook("qoder_pii_checker_risk_summary_hook", _HOOK_SCRIPT)
    findings = [
        {"severity": "deny", "type": "credential"},
        {"severity": "warn", "type": "email"},
        {"severity": "custom", "type": "custom"},
    ]

    assert hook._risk_summary("deny", findings) == (
        "Detected 3 sensitive data findings (2 high risk, 1 general risk)"
    )
    assert hook._risk_summary("warn", findings) == (
        "Detected 3 sensitive data findings (1 high risk, 2 general risk)"
    )
