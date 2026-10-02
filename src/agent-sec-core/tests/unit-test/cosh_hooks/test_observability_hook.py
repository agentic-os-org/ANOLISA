"""Unit tests for cosh-extension/hooks/observability_hook.py."""

import io
import json
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest
from standalone_hook_test_loader import load_standalone_hook

_COSH_EXTENSION_DIR = Path(__file__).resolve().parents[2] / ".." / "cosh-extension"
_COSH_HOOK = _COSH_EXTENSION_DIR / "hooks" / "observability_hook.py"

observability_hook = load_standalone_hook("cosh_observability_hook", _COSH_HOOK)

_TS = "2026-05-13T10:00:00Z"


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


def _json_size_bytes(value):
    return len(
        json.dumps(
            value,
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        ).encode("utf-8")
    )


def _record(input_data):
    record = observability_hook._build_record(input_data)
    assert record is not None
    return record


def _base(hook_event_name, **overrides):
    payload = {
        "hook_event_name": hook_event_name,
        "session_id": "session-123",
        "run_id": "run-123",
        "timestamp": _TS,
    }
    payload.update(overrides)
    return payload


def _assert_no_metrics(record, names):
    for name in names:
        assert name not in record["metrics"]


def test_user_prompt_submit_maps_prompt_and_uses_synthetic_run_id():
    record = _record(
        {
            "hook_event_name": "UserPromptSubmit",
            "session_id": "session-123",
            "timestamp": _TS,
            "prompt": "Summarize this repository.",
        }
    )

    assert record["hook"] == "before_agent_run"
    assert record["observedAt"] == _TS
    assert record["metadata"]["sessionId"] == "session-123"
    assert record["metadata"]["runId"].startswith("synthetic-run-")
    assert record["metrics"] == {
        "prompt": "Summarize this repository.",
        "user_input": "Summarize this repository.",
        "pii_scan_input_sha256": observability_hook.text_sha256(
            "Summarize this repository."
        ),
    }
    _assert_no_metrics(
        record,
        {
            "images_count",
            "context_window_utilization",
            "model_provider",
        },
    )


def test_before_model_maps_messages_and_model_fields_only():
    messages = [
        {"role": "system", "content": "Use concise answers."},
        {"role": "user", "content": "First request"},
        {"role": "model", "content": "First response"},
        {"role": "user", "content": "Second request"},
    ]
    record = _record(
        _base(
            "BeforeModel",
            llm_request={
                "model": "qwen-max",
                "messages": messages,
                "config": {"temperature": 0.2},
            },
        )
    )

    assert record["hook"] == "before_llm_call"
    assert record["metadata"] == {
        "sessionId": "session-123",
        "runId": "run-123",
    }
    assert record["metrics"] == {
        "prompt": messages,
        "system_prompt": ["Use concise answers."],
        "user_input": "Second request",
        "history_messages_count": 4,
        "model_id": "qwen-max",
    }
    _assert_no_metrics(
        record,
        {
            "images_count",
            "context_window_utilization",
            "model_provider",
            "api",
            "transport",
        },
    )


def test_after_model_maps_response_finish_reason_and_payload_sizes():
    llm_request = {
        "model": "qwen-max",
        "messages": [{"role": "user", "content": "Say hello"}],
    }
    llm_response = {
        "text": "Hello there.",
        "candidates": [
            {
                "content": {"role": "model", "parts": ["Hello ", "there."]},
                "finishReason": "STOP",
                "index": 0,
            }
        ],
    }

    record = _record(
        _base("AfterModel", llm_request=llm_request, llm_response=llm_response)
    )

    assert record["hook"] == "after_llm_call"
    assert record["metrics"] == {
        "outcome": "success",
        "response": "Hello there.",
        "stop_reason": "STOP",
        "assistant_texts_count": 2,
        "request_payload_bytes": _json_size_bytes(llm_request),
        "response_stream_bytes": _json_size_bytes(llm_response),
    }
    _assert_no_metrics(
        record,
        {
            "latency_ms",
            "time_to_first_byte_ms",
            "upstream_request_id_hash",
        },
    )


def test_pre_tool_use_maps_tool_fields_and_synthetic_tool_call_id():
    tool_input = {"command": "pwd"}
    record = _record(
        _base(
            "PreToolUse",
            tool_name="run_shell_command",
            tool_input=tool_input,
        )
    )

    assert record["hook"] == "before_tool_call"
    assert record["metadata"]["toolCallId"].startswith("synthetic-tool-")
    assert record["metrics"] == {
        "tool_name": "run_shell_command",
        "parameters": tool_input,
    }


def test_post_tool_use_maps_result_status_size_and_exit_code():
    tool_response = {"stdout": "ok\n", "exit_code": 0}
    record = _record(
        _base(
            "PostToolUse",
            tool_name="run_shell_command",
            tool_input={"command": "echo ok"},
            tool_use_id="tool-use-123",
            tool_response=tool_response,
        )
    )

    assert record["hook"] == "after_tool_call"
    assert record["metadata"]["toolCallId"] == "tool-use-123"
    assert record["metrics"] == {
        "result": tool_response,
        "status": "success",
        "pii_scan_input_sha256": observability_hook.text_sha256(
            observability_hook.value_to_text(tool_response)
        ),
        "result_size_bytes": _json_size_bytes(tool_response),
        "exit_code": 0,
    }
    _assert_no_metrics(record, {"duration_ms"})


@pytest.mark.parametrize(
    ("is_interrupt", "expected_status"),
    ((True, "interrupted"), (False, "error"), (None, "error")),
)
def test_post_tool_use_failure_maps_error_and_interrupt_status(
    is_interrupt, expected_status
):
    payload = _base(
        "PostToolUseFailure",
        tool_name="run_shell_command",
        tool_input={"command": "exit 1"},
        tool_use_id="tool-use-123",
        error="sandbox denied",
    )
    if is_interrupt is not None:
        payload["is_interrupt"] = is_interrupt

    record = _record(payload)

    assert record["hook"] == "after_tool_call"
    assert record["metadata"]["toolCallId"] == "tool-use-123"
    assert record["metrics"] == {
        "error": "sandbox denied",
        "status": expected_status,
        "pii_scan_input_sha256": observability_hook.text_sha256("sandbox denied"),
    }
    _assert_no_metrics(record, {"duration_ms", "result_size_bytes"})


@pytest.mark.parametrize(
    ("last_message", "output_kind", "assistant_texts_count"),
    (("Done.", "text", 1), ("", "empty", 0)),
)
def test_stop_maps_last_assistant_message(
    last_message, output_kind, assistant_texts_count
):
    record = _record(
        _base(
            "Stop",
            last_assistant_message=last_message,
        )
    )

    assert record["hook"] == "after_agent_run"
    assert record["metrics"] == {
        "response": last_message,
        "output_kind": output_kind,
        "assistant_texts_count": assistant_texts_count,
        "success": True,
    }
    _assert_no_metrics(
        record,
        {
            "duration_ms",
            "total_api_calls",
            "total_tool_calls",
            "final_model_id",
            "final_model_provider",
        },
    )


def test_build_record_returns_none_for_unsupported_hook():
    assert observability_hook._build_record(_base("BeforeToolSelection")) is None


def test_main_invokes_observability_cli_with_record(monkeypatch, capsys):
    calls = []

    def fake_run(cmd, **kwargs):
        calls.append((cmd, kwargs))
        if "scan-pii" in cmd:
            return SimpleNamespace(
                returncode=0,
                stdout=json.dumps({"redacted_text": kwargs["input"]}),
                stderr="",
            )
        return SimpleNamespace(returncode=0)

    monkeypatch.setattr(observability_hook.subprocess, "run", fake_run)
    monkeypatch.setattr(
        sys,
        "stdin",
        io.StringIO(json.dumps(_base("UserPromptSubmit", prompt="hello"))),
    )

    observability_hook.main()

    assert json.loads(capsys.readouterr().out) == {}
    assert len(calls) == 3
    cmd, kwargs = calls[-1]
    assert cmd == [
        "agent-sec-cli",
        "observability",
        "record",
        "--format",
        "json",
        "--stdin",
    ]
    assert kwargs["text"] is True
    assert json.loads(kwargs["input"])["hook"] == "before_agent_run"


def test_main_redacts_observability_payload_before_record(monkeypatch, capsys):
    calls = []

    def fake_run(cmd, **kwargs):
        calls.append((cmd, kwargs))
        if "scan-pii" in cmd:
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
        return SimpleNamespace(returncode=0)

    monkeypatch.setattr(observability_hook.subprocess, "run", fake_run)
    monkeypatch.setattr(
        sys,
        "stdin",
        io.StringIO(
            json.dumps(_base("UserPromptSubmit", prompt="email alice@example.com"))
        ),
    )

    observability_hook.main()

    assert json.loads(capsys.readouterr().out) == {}
    payload = json.loads(calls[-1][1]["input"])
    payload_text = json.dumps(payload, ensure_ascii=False)
    assert "alice@example.com" not in payload_text
    assert "a***@example.com" in payload_text


def test_main_invalid_json_returns_noop_without_cli(monkeypatch, capsys):
    def fail_run(*_args, **_kwargs):
        raise AssertionError("subprocess.run should not be called")

    monkeypatch.setattr(observability_hook.subprocess, "run", fail_run)
    monkeypatch.setattr(sys, "stdin", io.StringIO("not-json"))

    observability_hook.main()

    assert json.loads(capsys.readouterr().out) == {}


def test_main_oversized_payload_is_diagnosed_and_fail_open(monkeypatch, capsys):
    def fail_run(*_args, **_kwargs):
        raise AssertionError("subprocess.run should not be called")

    payload = b"alice@example.com" + b"x" * observability_hook._MAX_PAYLOAD_SIZE
    monkeypatch.setattr(observability_hook.subprocess, "run", fail_run)
    monkeypatch.setattr(sys, "stdin", SimpleNamespace(buffer=io.BytesIO(payload)))

    observability_hook.main()

    captured = capsys.readouterr()
    assert json.loads(captured.out) == {}
    assert captured.err == (
        "observability-hook: hook input exceeds "
        f"{observability_hook._MAX_PAYLOAD_SIZE} bytes; skipping event\n"
    )
    assert "alice@example.com" not in captured.err


def test_main_payload_at_size_limit_is_processed(monkeypatch, capsys):
    prefix = (
        b'{"hook_event_name":"UserPromptSubmit",'
        b'"session_id":"session-123","run_id":"run-123",'
        b'"prompt":"hello","padding":"'
    )
    suffix = b'"}'
    padding_size = observability_hook._MAX_PAYLOAD_SIZE - len(prefix) - len(suffix)
    payload = prefix + b"x" * padding_size + suffix
    records = []
    assert len(payload) == observability_hook._MAX_PAYLOAD_SIZE

    monkeypatch.setattr(observability_hook, "_record_observability", records.append)
    monkeypatch.setattr(sys, "stdin", SimpleNamespace(buffer=io.BytesIO(payload)))

    observability_hook.main()

    captured = capsys.readouterr()
    assert json.loads(captured.out) == {}
    assert captured.err == ""
    assert len(records) == 1
    assert records[0]["metrics"]["prompt"] == "hello"


@pytest.mark.parametrize(
    "subprocess_result",
    (
        SimpleNamespace(returncode=1),
        subprocess.TimeoutExpired(cmd=["agent-sec-cli"], timeout=1),
    ),
)
def test_main_cli_failure_and_timeout_return_noop(
    monkeypatch, capsys, subprocess_result
):
    def fake_run(*_args, **_kwargs):
        if isinstance(subprocess_result, Exception):
            raise subprocess_result
        return subprocess_result

    monkeypatch.setattr(observability_hook.subprocess, "run", fake_run)
    monkeypatch.setattr(
        sys,
        "stdin",
        io.StringIO(json.dumps(_base("UserPromptSubmit", prompt="hello"))),
    )

    observability_hook.main()

    assert json.loads(capsys.readouterr().out) == {}


def test_main_reports_missing_observability_cli(monkeypatch, capsys):
    def fake_run(*_args, **_kwargs):
        raise FileNotFoundError("agent-sec-cli")

    monkeypatch.setattr(observability_hook.subprocess, "run", fake_run)
    monkeypatch.setattr(
        sys,
        "stdin",
        io.StringIO(json.dumps(_base("UserPromptSubmit", prompt="hello"))),
    )

    observability_hook.main()

    captured = capsys.readouterr()
    assert json.loads(captured.out) == {}
    assert "agent-sec-cli executable was not found" in captured.err
    assert "install agent-sec-cli or add it to PATH" in captured.err


def test_main_reports_cli_stderr_for_invalid_record_payload(monkeypatch, capsys):
    def fake_run(*_args, **_kwargs):
        return SimpleNamespace(
            returncode=2,
            stderr="schema validation failed: metrics.status must be a string\n",
            stdout="",
        )

    monkeypatch.setattr(observability_hook.subprocess, "run", fake_run)
    monkeypatch.setattr(
        sys,
        "stdin",
        io.StringIO(json.dumps(_base("UserPromptSubmit", prompt="hello"))),
    )

    observability_hook.main()

    captured = capsys.readouterr()
    assert json.loads(captured.out) == {}
    assert "agent-sec-cli observability record failed with exit code 2" in captured.err
    assert "schema validation failed: metrics.status must be a string" in captured.err


def test_main_reports_observability_cli_timeout(monkeypatch, capsys):
    def fake_run(*_args, **_kwargs):
        raise subprocess.TimeoutExpired(
            cmd=["agent-sec-cli", "observability", "record"],
            timeout=3,
            stderr="partial validation output",
        )

    monkeypatch.setattr(observability_hook.subprocess, "run", fake_run)
    monkeypatch.setattr(
        sys,
        "stdin",
        io.StringIO(json.dumps(_base("UserPromptSubmit", prompt="hello"))),
    )

    observability_hook.main()

    captured = capsys.readouterr()
    assert json.loads(captured.out) == {}
    assert (
        "agent-sec-cli observability record timed out after 3 seconds" in captured.err
    )
    assert "partial validation output" in captured.err


def test_extension_registers_observability_hook_for_supported_events():
    config = json.loads((_COSH_EXTENSION_DIR / "cosh-extension.json").read_text())
    expected_timeouts = {
        "UserPromptSubmit": 15000,
        "BeforeModel": 20000,
        "AfterModel": 10000,
        "PreToolUse": 10000,
        "PostToolUse": 10000,
        "PostToolUseFailure": 10000,
        "Stop": 10000,
    }

    for event_name, expected_timeout in expected_timeouts.items():
        entries = config["hooks"].get(event_name, [])
        observability_hooks = [
            hook
            for entry in entries
            for hook in entry.get("hooks", [])
            if hook.get("name") == "observability-hook"
        ]
        assert len(observability_hooks) == 1
        assert observability_hooks[0]["command"] == (
            "python3 ${extensionPath}/hooks/observability_hook.py"
        )
        assert observability_hooks[0]["timeout"] == expected_timeout
