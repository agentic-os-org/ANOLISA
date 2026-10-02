"""Unit tests for the Qoder observability command hook."""

import builtins
import io
import json
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest
from agent_sec_cli.observability.schema import validate_observability_record
from standalone_hook_test_loader import load_standalone_hook

_ROOT = Path(__file__).resolve().parents[3]
_PLUGIN_DIR = _ROOT / "qoder-plugin"
_HOOKS_DIR = _PLUGIN_DIR / "hooks"
_HOOK_PATH = _HOOKS_DIR / "observability_hook.py"

observability_hook = load_standalone_hook("qoder_observability_hook", _HOOK_PATH)

_TS = "2026-07-20T08:00:00Z"
_ZERO_RUN_ID = "00000000-0000-0000-0000-000000000000"
_CONTEXT = {
    "agent_name": "qoder",
    "session_id": "session-123",
}


@pytest.fixture(autouse=True)
def _fixed_runtime(monkeypatch) -> None:
    monkeypatch.setattr(observability_hook, "_now_iso", lambda: _TS)


@pytest.mark.parametrize(
    ("value", "expected"),
    [
        (None, 5),
        ("", 5),
        ("invalid", 5),
        ("0", 5),
        ("-1", 5),
        ("3", 3),
        ("5", 5),
        ("7", 5),
        ("999999", 5),
    ],
)
def test_observability_timeout_environment(monkeypatch, value, expected):
    if value is None:
        monkeypatch.delenv("OBSERVABILITY_TIMEOUT", raising=False)
    else:
        monkeypatch.setenv("OBSERVABILITY_TIMEOUT", value)

    assert observability_hook._read_cli_timeout_seconds() == expected


def _base(hook_event_name: str, **overrides):
    payload = {
        "hook_event_name": hook_event_name,
        "session_id": "session-123",
        "transcript_path": "/private/qoder-transcript.jsonl",
        "cwd": "/workspace",
        "permission_mode": "default",
        "agent_id": "agent-1",
        "agent_type": "main",
    }
    payload.update(overrides)
    return payload


def _record(input_data):
    record = observability_hook._build_record(input_data, dict(_CONTEXT))
    assert record is not None
    return record


def test_user_prompt_submit_maps_before_agent_run() -> None:
    prompt = "Summarize this repository."

    record = _record(_base("UserPromptSubmit", prompt=prompt))

    assert record == {
        "hook": "before_agent_run",
        "observedAt": _TS,
        "metadata": {
            "sessionId": "session-123",
            "runId": _ZERO_RUN_ID,
        },
        "metrics": {
            "prompt": prompt,
            "user_input": prompt,
            "pii_scan_input_sha256": observability_hook._pii_scan_input_sha256(prompt),
        },
    }


@pytest.mark.parametrize("prompt", (None, "", "  \n"))
def test_user_prompt_submit_skips_empty_prompt(prompt) -> None:
    payload = _base("UserPromptSubmit")
    if prompt is not None:
        payload["prompt"] = prompt

    assert observability_hook._build_record(payload, dict(_CONTEXT)) is None


def test_pre_tool_use_maps_tool_metadata_and_parameters() -> None:
    tool_input = {"command": "pwd"}
    context = {**_CONTEXT, "tool_call_id": "tool-123"}

    record = observability_hook._build_record(
        _base(
            "PreToolUse",
            tool_name="Bash",
            tool_input=tool_input,
            tool_use_id="tool-123",
        ),
        context,
    )

    assert record is not None
    assert record["hook"] == "before_tool_call"
    assert record["metadata"] == {
        "sessionId": "session-123",
        "runId": _ZERO_RUN_ID,
        "toolCallId": "tool-123",
    }
    assert record["metrics"] == {
        "tool_name": "Bash",
        "parameters": tool_input,
        "pii_scan_input_sha256": observability_hook._pii_scan_input_sha256(tool_input),
    }


@pytest.mark.parametrize(
    ("exit_code", "expected_status"),
    ((0, "success"), (2, "error")),
)
def test_post_tool_use_maps_result_exit_code_and_status(
    exit_code: int, expected_status: str
) -> None:
    response = {"stdout": "done\n", "exit_code": exit_code}
    context = {**_CONTEXT, "tool_call_id": "tool-123"}

    record = observability_hook._build_record(
        _base(
            "PostToolUse",
            tool_name="Bash",
            tool_response=response,
            tool_use_id="tool-123",
        ),
        context,
    )

    assert record is not None
    assert record["hook"] == "after_tool_call"
    assert record["metadata"]["toolCallId"] == "tool-123"
    assert record["metrics"] == {
        "status": expected_status,
        "result": response,
        "result_size_bytes": observability_hook._json_size_bytes(response),
        "pii_scan_input_sha256": observability_hook._pii_scan_input_sha256(response),
        "exit_code": exit_code,
    }


@pytest.mark.parametrize(
    ("is_interrupt", "expected_status"),
    ((True, "interrupted"), (False, "error"), (None, "error")),
)
def test_post_tool_use_failure_maps_error(
    is_interrupt: bool | None, expected_status: str
) -> None:
    payload = _base(
        "PostToolUseFailure",
        tool_name="Bash",
        tool_use_id="tool-123",
        error="command failed",
        error_type="execution_failed",
    )
    if is_interrupt is not None:
        payload["is_interrupt"] = is_interrupt
    context = {**_CONTEXT, "tool_call_id": "tool-123"}

    record = observability_hook._build_record(payload, context)

    assert record is not None
    assert record["hook"] == "after_tool_call"
    assert record["metrics"]["status"] == expected_status
    assert record["metrics"]["error"] == "command failed"


def test_stop_maps_successful_agent_run() -> None:
    record = _record(
        _base("Stop", stop_hook_active=False, last_assistant_message="Done.")
    )

    assert record["hook"] == "after_agent_run"
    assert record["metrics"] == {
        "response": "Done.",
        "output_kind": "text",
        "assistant_texts_count": 1,
        "success": True,
    }


def test_stop_failure_prefers_error_details_and_maps_partial_response() -> None:
    record = _record(
        _base(
            "StopFailure",
            error="rate_limit",
            error_details="rate limit exceeded",
            last_assistant_message="Partial answer",
        )
    )

    assert record["hook"] == "after_agent_run"
    assert record["metrics"] == {
        "error": "rate limit exceeded",
        "output_kind": "text",
        "assistant_texts_count": 1,
        "success": False,
        "response": "Partial answer",
    }


def test_stop_failure_falls_back_to_required_error_category() -> None:
    record = _record(_base("StopFailure", error="rate_limit"))

    assert record["hook"] == "after_agent_run"
    assert record["metrics"] == {
        "error": "rate_limit",
        "output_kind": "empty",
        "assistant_texts_count": 0,
        "success": False,
    }


def test_stop_failure_normalizes_malformed_optional_fields() -> None:
    record = _record(
        _base(
            "StopFailure",
            error={"category": "rate_limit"},
            error_details=["unexpected shape"],
            last_assistant_message={"text": "unexpected shape"},
        )
    )

    assert record["hook"] == "after_agent_run"
    assert record["metrics"] == {
        "error": "unknown",
        "output_kind": "empty",
        "assistant_texts_count": 0,
        "success": False,
    }


@pytest.mark.parametrize("event_name", ("BeforeModel", "AfterModel", "SessionStart"))
def test_events_without_schema_mapping_are_skipped(event_name: str) -> None:
    assert observability_hook._build_record(_base(event_name), dict(_CONTEXT)) is None


@pytest.mark.parametrize("event_name", ([], {}))
def test_malformed_event_name_is_skipped_without_raising(event_name) -> None:
    assert observability_hook._build_record(_base(event_name), dict(_CONTEXT)) is None


def test_missing_required_correlation_is_skipped() -> None:
    assert (
        observability_hook._build_record(
            _base("UserPromptSubmit", prompt="hello"),
            {"agent_name": "qoder"},
        )
        is None
    )
    assert (
        observability_hook._build_record(
            _base("PreToolUse", tool_name="Bash", tool_input={}),
            dict(_CONTEXT),
        )
        is None
    )


def test_qoder_only_fields_are_not_added_to_current_schema_metadata() -> None:
    record = _record(_base("UserPromptSubmit", prompt="hello"))

    assert record["metadata"] == {
        "sessionId": "session-123",
        "runId": _ZERO_RUN_ID,
    }
    serialized = json.dumps(record)
    assert "cwd" not in serialized
    assert "agent_id" not in serialized
    assert "transcript_path" not in serialized


def test_all_six_qoder_events_build_schema_valid_records() -> None:
    tool_context = {**_CONTEXT, "tool_call_id": "tool-123"}
    cases = [
        (_base("UserPromptSubmit", prompt="hello"), _CONTEXT),
        (
            _base(
                "PreToolUse",
                tool_name="Bash",
                tool_input={"command": "pwd"},
                tool_use_id="tool-123",
            ),
            tool_context,
        ),
        (
            _base(
                "PostToolUse",
                tool_name="Bash",
                tool_response={"stdout": "ok"},
                tool_use_id="tool-123",
            ),
            tool_context,
        ),
        (
            _base(
                "PostToolUseFailure",
                tool_name="Bash",
                error="failed",
                tool_use_id="tool-123",
            ),
            tool_context,
        ),
        (_base("Stop", last_assistant_message="done"), _CONTEXT),
        (
            _base("StopFailure", error="unknown"),
            _CONTEXT,
        ),
    ]

    records = []
    for payload, context in cases:
        record = observability_hook._build_record(payload, dict(context))
        assert record is not None
        records.append(validate_observability_record(record).to_record())

    assert {record["hook"] for record in records} == {
        "before_agent_run",
        "before_tool_call",
        "after_tool_call",
        "after_agent_run",
    }


def test_main_redacts_and_uses_host_trace_context_for_both_cli_calls(
    monkeypatch, capsys
) -> None:
    calls = []

    def fake_run(command, **kwargs):
        calls.append((command, kwargs))
        if "scan-pii" in command:
            return SimpleNamespace(
                returncode=0,
                stdout=json.dumps(
                    {
                        "redacted_text": kwargs["input"].replace(
                            "alice@example.com", "a***@example.com"
                        )
                    }
                ),
                stderr="",
            )
        return SimpleNamespace(returncode=0, stdout="", stderr="")

    monkeypatch.setattr(observability_hook.subprocess, "run", fake_run)
    monkeypatch.setattr(
        sys,
        "stdin",
        io.StringIO(
            json.dumps(
                _base(
                    "UserPromptSubmit",
                    prompt="contact alice@example.com",
                )
            )
        ),
    )

    observability_hook.main()

    captured = capsys.readouterr()
    assert captured.out == ""
    assert captured.err == ""
    assert len(calls) == 2

    contexts = []
    for command, _kwargs in calls:
        context_index = command.index("--trace-context") + 1
        contexts.append(json.loads(command[context_index]))
    assert "--include-low-confidence" in calls[0][0]
    assert (
        contexts[0]
        == contexts[1]
        == {
            "agent_name": "qoder",
            "session_id": "session-123",
        }
    )

    record_payload = json.loads(calls[-1][1]["input"])
    assert record_payload["metadata"]["runId"] == _ZERO_RUN_ID
    serialized = json.dumps(record_payload, ensure_ascii=False)
    assert "alice@example.com" not in serialized
    assert "a***@example.com" in serialized


def test_redaction_failure_drops_raw_value_and_reports_once(
    monkeypatch, capsys
) -> None:
    monkeypatch.setattr(
        observability_hook,
        "_redact_text",
        lambda _text, _context: None,
    )
    record = _record(_base("UserPromptSubmit", prompt="alice@example.com"))

    safe_record = observability_hook._redact_observability_record(
        record, dict(_CONTEXT)
    )

    assert "prompt" not in safe_record["metrics"]
    assert "user_input" not in safe_record["metrics"]
    assert safe_record["metrics"]["pii_scan_input_sha256"]
    captured = capsys.readouterr()
    assert captured.out == ""
    assert captured.err == (
        "qoder-observability-hook: PII redaction failed; sensitive metric dropped\n"
    )
    assert "alice@example.com" not in captured.err


@pytest.mark.parametrize("payload", ("not-json", "[]"))
def test_invalid_input_is_fail_open_without_cli(
    payload: str, monkeypatch, capsys
) -> None:
    def fail_run(*_args, **_kwargs):
        raise AssertionError("subprocess.run should not be called")

    monkeypatch.setattr(observability_hook.subprocess, "run", fail_run)
    monkeypatch.setattr(sys, "stdin", io.StringIO(payload))

    observability_hook.main()

    captured = capsys.readouterr()
    assert captured.out == ""
    assert captured.err == ""


def test_unexpected_input_error_is_fail_open_without_leaking_details(
    monkeypatch, capsys
) -> None:
    def fail_read():
        raise RecursionError("alice@example.com")

    monkeypatch.setattr(observability_hook, "_read_stdin_payload", fail_read)

    observability_hook.main()

    captured = capsys.readouterr()
    assert captured.out == ""
    assert captured.err == (
        "qoder-observability-hook: unexpected RecursionError while processing unknown\n"
    )
    assert "alice@example.com" not in captured.err


def test_unexpected_processing_error_is_fail_open_without_leaking_payload(
    monkeypatch, capsys
) -> None:
    def fail_build(_input_data, _context):
        raise RuntimeError("alice@example.com")

    monkeypatch.setattr(observability_hook, "_build_record", fail_build)
    monkeypatch.setattr(
        sys,
        "stdin",
        io.StringIO(
            json.dumps(
                _base(
                    "StopFailure",
                    error="rate_limit",
                    error_details="alice@example.com",
                )
            )
        ),
    )

    observability_hook.main()

    captured = capsys.readouterr()
    assert captured.out == ""
    assert captured.err == (
        "qoder-observability-hook: unexpected RuntimeError while processing StopFailure\n"
    )
    assert "alice@example.com" not in captured.err


def test_diagnostic_write_failure_is_fail_open(monkeypatch) -> None:
    def fail_print(*_args, **_kwargs):
        raise BrokenPipeError("stderr is closed")

    with monkeypatch.context() as patch:
        patch.setattr(builtins, "print", fail_print)
        observability_hook._diagnostic("ignored")


def test_failure_reporting_failure_is_fail_open(monkeypatch) -> None:
    def fail_read():
        raise RecursionError("input failed")

    def fail_event_name(_input_data):
        raise TypeError("diagnostic failed")

    monkeypatch.setattr(observability_hook, "_read_stdin_payload", fail_read)
    monkeypatch.setattr(observability_hook, "_diagnostic_event_name", fail_event_name)

    observability_hook.main()


def test_oversized_input_is_diagnosed_without_leaking_payload(
    monkeypatch, capsys
) -> None:
    payload = b"alice@example.com" + b"x" * observability_hook._MAX_PAYLOAD_SIZE
    monkeypatch.setattr(sys, "stdin", SimpleNamespace(buffer=io.BytesIO(payload)))

    observability_hook.main()

    captured = capsys.readouterr()
    assert captured.out == ""
    assert captured.err == (
        "qoder-observability-hook: hook input exceeds "
        f"{observability_hook._MAX_PAYLOAD_SIZE} bytes; skipping event\n"
    )
    assert "alice@example.com" not in captured.err


def test_record_timeout_is_fail_open(monkeypatch, capsys) -> None:
    monkeypatch.setattr(
        observability_hook,
        "_redact_text",
        lambda text, _context: text,
    )

    def timeout(*_args, **_kwargs):
        raise subprocess.TimeoutExpired(
            cmd=["agent-sec-cli", "observability", "record"],
            timeout=3,
            stderr="record timeout",
        )

    monkeypatch.setattr(observability_hook.subprocess, "run", timeout)
    observability_hook._record_observability(
        _record(_base("Stop", last_assistant_message="Done.")),
        dict(_CONTEXT),
    )

    assert "timed out after 3 seconds" in capsys.readouterr().err


def test_hooks_config_registers_observability_for_all_supported_events() -> None:
    config = json.loads((_HOOKS_DIR / "hooks.json").read_text())
    expected_events = {
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "PostToolUseFailure",
        "Stop",
        "StopFailure",
    }
    expected_args = ["${QODER_PLUGIN_ROOT}/hooks/observability_hook.py"]

    assert set(config["hooks"]) == expected_events
    for event_name in expected_events:
        first_group_names = {
            hook.get("name") for hook in config["hooks"][event_name][0]["hooks"]
        }
        assert first_group_names == {"agent-sec-observability"}
        matching = [
            hook
            for group in config["hooks"][event_name]
            for hook in group["hooks"]
            if hook.get("name") == "agent-sec-observability"
        ]
        assert len(matching) == 1
        hook = matching[0]
        assert hook["command"] == "python3"
        assert hook["args"] == expected_args
        assert hook["timeout"] == 10
        assert hook["async"] is True
