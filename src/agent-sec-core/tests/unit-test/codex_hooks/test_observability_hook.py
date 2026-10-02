"""Unit tests for codex-plugin/hooks/observability_hook.py."""

import io
import json
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest
from standalone_hook_test_loader import load_standalone_hook

_CODEX_PLUGIN_DIR = Path(__file__).resolve().parents[2] / ".." / "codex-plugin"
_HOOK = _CODEX_PLUGIN_DIR / "hooks-plugin" / "hooks" / "observability_hook.py"
observability_hook = load_standalone_hook("codex_observability_hook", _HOOK)

_TS = "2026-07-15T08:00:00Z"


@pytest.fixture(autouse=True)
def _fixed_timestamp(monkeypatch):
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


def _base(hook_event_name, **overrides):
    payload = {
        "hook_event_name": hook_event_name,
        "session_id": "session-123",
        "turn_id": "turn-123",
        "model": "gpt-5",
    }
    payload.update(overrides)
    return payload


def _record(input_data):
    record = observability_hook._build_record(input_data)
    assert record is not None
    return record


def test_user_prompt_submit_maps_turn_metadata_and_metrics():
    prompt = "Summarize this change."
    record = _record(_base("UserPromptSubmit", prompt=prompt))

    assert record == {
        "hook": "before_agent_run",
        "observedAt": _TS,
        "metadata": {
            "sessionId": "session-123",
            "runId": "turn-123",
        },
        "metrics": {
            "prompt": prompt,
            "user_input": prompt,
            "pii_scan_input_sha256": observability_hook._pii_scan_input_sha256(prompt),
            "model_id": "gpt-5",
        },
    }


@pytest.mark.parametrize(
    "missing_field",
    ("session_id", "turn_id"),
)
def test_agent_record_skips_missing_required_correlation_field(missing_field, capsys):
    payload = _base("UserPromptSubmit", prompt="hello")
    payload.pop(missing_field)

    assert observability_hook._build_record(payload) is None
    assert missing_field in capsys.readouterr().err


def test_pre_tool_use_maps_all_three_correlation_levels():
    tool_input = {"command": "pwd"}
    record = _record(
        _base(
            "PreToolUse",
            tool_name="Bash",
            tool_input=tool_input,
            tool_use_id="tool-123",
        )
    )

    assert record["hook"] == "before_tool_call"
    assert record["metadata"] == {
        "sessionId": "session-123",
        "runId": "turn-123",
        "toolCallId": "tool-123",
    }
    assert record["metrics"] == {
        "tool_name": "Bash",
        "parameters": tool_input,
        "pii_scan_input_sha256": observability_hook._pii_scan_input_sha256(tool_input),
    }


def test_tool_record_skips_missing_tool_use_id(capsys):
    payload = _base("PreToolUse", tool_name="Bash", tool_input={"command": "pwd"})

    assert observability_hook._build_record(payload) is None
    assert "tool_use_id" in capsys.readouterr().err


def test_record_trace_context_maps_tool_metadata():
    record = _record(
        _base(
            "PreToolUse",
            tool_name="Bash",
            tool_input={"command": "pwd"},
            tool_use_id="tool-123",
        )
    )

    assert observability_hook._record_trace_context(record) == {
        "agent_name": "codex",
        "session_id": "session-123",
        "run_id": "turn-123",
        "tool_call_id": "tool-123",
    }


@pytest.mark.parametrize(
    ("exit_code", "expected_status"),
    ((0, "success"), (2, "error")),
)
def test_post_tool_use_maps_result_size_exit_code_and_status(
    exit_code, expected_status
):
    tool_response = {"stdout": "done\n", "exit_code": exit_code}
    record = _record(
        _base(
            "PostToolUse",
            tool_name="Bash",
            tool_use_id="tool-123",
            tool_response=tool_response,
        )
    )

    assert record["hook"] == "after_tool_call"
    assert record["metadata"]["toolCallId"] == "tool-123"
    assert record["metrics"] == {
        "result": tool_response,
        "result_size_bytes": observability_hook._json_size_bytes(tool_response),
        "pii_scan_input_sha256": observability_hook._pii_scan_input_sha256(
            tool_response
        ),
        "exit_code": exit_code,
        "status": expected_status,
    }


def test_post_tool_use_does_not_invent_status_without_exit_code():
    record = _record(
        _base(
            "PostToolUse",
            tool_use_id="tool-123",
            tool_response="completed",
        )
    )

    assert "status" not in record["metrics"]
    assert "duration_ms" not in record["metrics"]


@pytest.mark.parametrize(
    ("message", "output_kind", "text_count"),
    (("Done.", "text", 1), ("", "empty", 0)),
)
def test_stop_maps_final_response_without_llm_metrics(message, output_kind, text_count):
    record = _record(_base("Stop", last_assistant_message=message))

    assert record["hook"] == "after_agent_run"
    assert record["metrics"] == {
        "response": message,
        "output_kind": output_kind,
        "assistant_texts_count": text_count,
        "final_model_id": "gpt-5",
    }
    assert "total_api_calls" not in record["metrics"]
    assert "duration_ms" not in record["metrics"]


def test_build_record_rejects_unsupported_and_model_events():
    assert observability_hook._build_record(_base("PermissionRequest")) is None
    assert observability_hook._build_record(_base("BeforeModel")) is None
    assert observability_hook._build_record(_base("AfterModel")) is None


def test_redaction_caches_duplicate_sensitive_values(monkeypatch):
    calls = []

    def fake_redact(text, _security_trace_context):
        calls.append(text)
        return text.replace("alice@example.com", "a***@example.com")

    monkeypatch.setattr(observability_hook, "_redact_text", fake_redact)
    record = _record(_base("UserPromptSubmit", prompt="contact alice@example.com"))

    safe_record = observability_hook._redact_observability_record(record)

    assert calls == ["contact alice@example.com"]
    serialized = json.dumps(safe_record, ensure_ascii=False)
    assert "alice@example.com" not in serialized
    assert "a***@example.com" in serialized


def test_redaction_failure_drops_raw_value_but_keeps_hash_and_model(monkeypatch):
    monkeypatch.setattr(
        observability_hook,
        "_redact_text",
        lambda _text, _security_trace_context: None,
    )
    record = _record(_base("UserPromptSubmit", prompt="contact alice@example.com"))

    safe_record = observability_hook._redact_observability_record(record)

    assert "prompt" not in safe_record["metrics"]
    assert "user_input" not in safe_record["metrics"]
    assert "pii_scan_input_sha256" in safe_record["metrics"]
    assert safe_record["metrics"]["model_id"] == "gpt-5"


def test_main_redacts_then_records_and_returns_noop(monkeypatch, capsys):
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
        return SimpleNamespace(returncode=0, stdout="", stderr="")

    monkeypatch.setattr(observability_hook.subprocess, "run", fake_run)
    monkeypatch.setattr(
        sys,
        "stdin",
        io.StringIO(
            json.dumps(_base("UserPromptSubmit", prompt="contact alice@example.com"))
        ),
    )

    observability_hook.main()

    assert json.loads(capsys.readouterr().out) == {}
    assert len(calls) == 2
    pii_command = calls[0][0]
    trace_context_index = pii_command.index("--trace-context") + 1
    assert json.loads(pii_command[trace_context_index]) == {
        "agent_name": "codex",
        "session_id": "session-123",
        "run_id": "turn-123",
    }
    command, kwargs = calls[-1]
    assert command == [
        "agent-sec-cli",
        "observability",
        "record",
        "--format",
        "json",
        "--stdin",
    ]
    payload = json.loads(kwargs["input"])
    assert payload["metadata"]["runId"] == "turn-123"
    assert "alice@example.com" not in json.dumps(payload, ensure_ascii=False)


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
        b'"session_id":"session-123","turn_id":"turn-123",'
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


def test_main_observability_cli_failure_is_fail_open(monkeypatch, capsys):
    monkeypatch.setattr(
        observability_hook,
        "_redact_text",
        lambda text, _security_trace_context: text,
    )

    def fail_run(*_args, **_kwargs):
        raise FileNotFoundError("agent-sec-cli")

    monkeypatch.setattr(observability_hook.subprocess, "run", fail_run)
    monkeypatch.setattr(
        sys,
        "stdin",
        io.StringIO(json.dumps(_base("Stop", last_assistant_message="Done."))),
    )

    observability_hook.main()

    captured = capsys.readouterr()
    assert json.loads(captured.out) == {}
    assert "agent-sec-cli executable was not found" in captured.err


def test_record_timeout_is_fail_open(monkeypatch, capsys):
    monkeypatch.setattr(
        observability_hook,
        "_redact_text",
        lambda text, _security_trace_context: text,
    )

    def timeout(*_args, **_kwargs):
        raise subprocess.TimeoutExpired(
            cmd=["agent-sec-cli", "observability", "record"],
            timeout=3,
            stderr="record timeout",
        )

    monkeypatch.setattr(observability_hook.subprocess, "run", timeout)

    observability_hook._record_observability(
        _record(_base("Stop", last_assistant_message="Done."))
    )

    captured = capsys.readouterr()
    assert "timed out after 3 seconds" in captured.err
    assert "record timeout" in captured.err


def test_hooks_config_registers_observability_for_four_codex_events():
    config = json.loads(
        (_CODEX_PLUGIN_DIR / "hooks-plugin" / "hooks" / "hooks.json").read_text()
    )
    expected_events = {
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "Stop",
    }
    command = "python3 ${PLUGIN_ROOT}/hooks/observability_hook.py"

    registered_events = set()
    for event_name, entries in config["hooks"].items():
        matching = [
            (entry, hook)
            for entry in entries
            for hook in entry.get("hooks", [])
            if hook.get("command") == command
        ]
        if matching:
            registered_events.add(event_name)
            assert len(matching) == 1
            entry, hook = matching[0]
            assert entry.get("matcher") in (None, "", "*")
            assert hook["timeout"] == 10

    assert registered_events == expected_events
