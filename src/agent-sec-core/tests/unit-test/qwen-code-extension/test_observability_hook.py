"""Unit tests for the Qwen Code observability command hook."""

import io
import json
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest
from standalone_hook_test_loader import load_standalone_hook

_ROOT = Path(__file__).resolve().parents[3]
_EXTENSION_DIR = _ROOT / "qwen-code-extension"
_HOOK_PATH = _EXTENSION_DIR / "hooks" / "observability_hook.py"

observability_hook = load_standalone_hook("qwen_observability_hook", _HOOK_PATH)

_TS = "2026-07-14T10:00:00Z"


@pytest.fixture(autouse=True)
def _qwen_environment(monkeypatch):
    monkeypatch.setenv("QWEN_CODE_SESSION_ID", "env-session")
    monkeypatch.setenv("QWEN_CODE_PROMPT_ID", "prompt-run-123")


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
        "transcript_path": "/tmp/qwen-transcript.jsonl",
        "cwd": "/workspace",
        "timestamp": _TS,
    }
    payload.update(overrides)
    return payload


def _record(input_data):
    record = observability_hook._build_record(input_data)
    assert record is not None
    assert record["metadata"]["runId"] == observability_hook._ZERO_RUN_ID
    return record


def test_user_prompt_submit_maps_prompt_with_zero_run_id():
    record = _record(_base("UserPromptSubmit", prompt="Summarize this repository."))

    assert record == {
        "hook": "before_agent_run",
        "observedAt": _TS,
        "metadata": {
            "sessionId": "session-123",
            "runId": observability_hook._ZERO_RUN_ID,
        },
        "metrics": {
            "prompt": "Summarize this repository.",
            "user_input": "Summarize this repository.",
            "pii_scan_input_sha256": observability_hook.text_sha256(
                "Summarize this repository."
            ),
        },
    }


@pytest.mark.parametrize("prompt", (None, "", "   \n"))
def test_user_prompt_submit_skips_empty_tool_result_reentry(prompt):
    payload = _base("UserPromptSubmit")
    if prompt is not None:
        payload["prompt"] = prompt

    assert observability_hook._build_record(payload) is None


def test_run_id_ignores_prompt_context():
    record = _record(_base("UserPromptSubmit", prompt="hello"))

    assert record["metadata"] == {
        "sessionId": "session-123",
        "runId": observability_hook._ZERO_RUN_ID,
    }


def test_session_id_falls_back_to_qwen_environment():
    payload = _base("UserPromptSubmit", prompt="hello")
    payload.pop("session_id")

    record = _record(payload)

    assert record["metadata"]["sessionId"] == "env-session"


def test_pre_tool_use_prefers_tool_call_id_and_hashes_parameters():
    tool_input = {"command": "pwd"}
    record = _record(
        _base(
            "PreToolUse",
            permission_mode="default",
            tool_name="run_shell_command",
            tool_input=tool_input,
            tool_call_id="tool-call-123",
            tool_use_id="tool-use-123",
        )
    )

    assert record["hook"] == "before_tool_call"
    assert record["metadata"]["toolCallId"] == "tool-call-123"
    assert record["metrics"] == {
        "tool_name": "run_shell_command",
        "parameters": tool_input,
        "pii_scan_input_sha256": observability_hook.text_sha256(
            observability_hook.value_to_text(tool_input)
        ),
    }


def test_observability_metadata_uses_bounded_shared_trace_context():
    record = _record(
        _base(
            "PreToolUse",
            tool_name="run_shell_command",
            tool_input={"command": "pwd"},
            session_id="s" * 300,
            tool_call_id="t" * 300,
        )
    )

    assert record["metadata"]["sessionId"] == "s" * 256
    assert record["metadata"]["toolCallId"] == "t" * 256


def test_post_tool_use_maps_nested_exit_code_and_result():
    tool_response = {
        "llmContent": "ok",
        "returnDisplay": {"stdout": "ok\n", "exitCode": 0},
    }
    record = _record(
        _base(
            "PostToolUse",
            permission_mode="default",
            tool_name="run_shell_command",
            tool_input={"command": "echo ok"},
            tool_use_id="tool-use-123",
            tool_response=tool_response,
        )
    )

    assert record["hook"] == "after_tool_call"
    assert record["metadata"]["toolCallId"] == "tool-use-123"
    assert record["metrics"]["status"] == "success"
    assert record["metrics"]["result"] == tool_response
    assert record["metrics"]["exit_code"] == 0
    assert record["metrics"]["result_size_bytes"] > 0


@pytest.mark.parametrize(
    ("is_interrupt", "expected_status"),
    ((True, "interrupted"), (False, "error"), (None, "error")),
)
def test_post_tool_use_failure_maps_status(is_interrupt, expected_status):
    payload = _base(
        "PostToolUseFailure",
        permission_mode="default",
        tool_name="run_shell_command",
        tool_input={"command": "exit 1"},
        tool_use_id="tool-use-123",
        error="sandbox denied",
    )
    if is_interrupt is not None:
        payload["is_interrupt"] = is_interrupt

    record = _record(payload)

    assert record["hook"] == "after_tool_call"
    assert record["metrics"]["status"] == expected_status
    assert record["metrics"]["error"] == "sandbox denied"


def test_stop_maps_successful_agent_run():
    record = _record(
        _base(
            "Stop",
            stop_hook_active=False,
            last_assistant_message="Done.",
            context_usage=0.4,
            context_limit=131072,
            input_tokens=52428,
            background_tasks=[],
            crons=[],
        )
    )

    assert record["hook"] == "after_agent_run"
    assert record["metrics"] == {
        "response": "Done.",
        "output_kind": "text",
        "assistant_texts_count": 1,
        "success": True,
    }


def test_stop_failure_maps_error_details():
    record = _record(
        _base(
            "StopFailure",
            error="TURN_LIMIT",
            error_details="maximum turns reached",
            last_assistant_message="Partial answer",
        )
    )

    assert record["hook"] == "after_agent_run"
    assert record["metrics"] == {
        "error": "maximum turns reached",
        "output_kind": "text",
        "success": False,
        "response": "Partial answer",
        "assistant_texts_count": 1,
        "stop_reason": "TURN_LIMIT",
    }


def test_missing_required_correlation_returns_none(monkeypatch):
    monkeypatch.delenv("QWEN_CODE_SESSION_ID")
    assert (
        observability_hook._build_record(
            {
                "hook_event_name": "UserPromptSubmit",
                "timestamp": _TS,
                "prompt": "hello",
            }
        )
        is None
    )
    assert (
        observability_hook._build_record(
            _base("PreToolUse", tool_name="shell", tool_input={})
        )
        is None
    )


@pytest.mark.parametrize(
    "event_name",
    ("SessionStart", "SessionEnd", "PreCompact", "Notification"),
)
def test_official_events_without_observability_schema_mapping_are_skipped(event_name):
    assert observability_hook._build_record(_base(event_name)) is None


def test_main_redacts_payload_and_emits_no_decision(monkeypatch, capsys):
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
            json.dumps(_base("UserPromptSubmit", prompt="email alice@example.com"))
        ),
    )

    observability_hook.main()

    assert json.loads(capsys.readouterr().out) == {}
    payload = json.loads(calls[-1][1]["input"])
    payload_text = json.dumps(payload, ensure_ascii=False)
    assert "alice@example.com" not in payload_text
    assert "a***@example.com" in payload_text
    assert calls[-1][0] == observability_hook._OBSERVABILITY_COMMAND
    pii_call = next(call for call in calls if "scan-pii" in call[0])
    assert pii_call[0] == [
        "agent-sec-cli",
        "--trace-context",
        json.dumps(
            {"agent_name": "qwen-code", "session_id": "session-123"},
            ensure_ascii=False,
            separators=(",", ":"),
        ),
        "scan-pii",
        "--stdin",
        "--format",
        "json",
        "--redact-output",
        "--source",
        "observability",
    ]


def test_main_skips_empty_tool_result_reentry_without_local_state(monkeypatch, capsys):
    records = []
    monkeypatch.setattr(observability_hook, "_record_observability", records.append)

    payloads = [
        _base("UserPromptSubmit", prompt="first user prompt"),
        _base("UserPromptSubmit", prompt=""),
        _base("Stop", last_assistant_message="Done."),
        _base("UserPromptSubmit", prompt="next user prompt"),
    ]
    for payload in payloads:
        monkeypatch.setattr(sys, "stdin", io.StringIO(json.dumps(payload)))
        observability_hook.main()

    assert [record["hook"] for record in records] == [
        "before_agent_run",
        "after_agent_run",
        "before_agent_run",
    ]
    assert [
        record["metrics"]["prompt"]
        for record in records
        if record["hook"] == "before_agent_run"
    ] == [
        "first user prompt",
        "next user prompt",
    ]
    assert [json.loads(line) for line in capsys.readouterr().out.splitlines()] == [
        {},
        {},
        {},
        {},
    ]


def test_main_records_user_prompts_from_each_session(monkeypatch, capsys):
    records = []
    monkeypatch.setattr(observability_hook, "_record_observability", records.append)

    for session_id in ("session-a", "session-b"):
        payload = _base("UserPromptSubmit", session_id=session_id, prompt=session_id)
        monkeypatch.setattr(sys, "stdin", io.StringIO(json.dumps(payload)))
        observability_hook.main()

    assert [record["metadata"]["sessionId"] for record in records] == [
        "session-a",
        "session-b",
    ]
    assert [json.loads(line) for line in capsys.readouterr().out.splitlines()] == [
        {},
        {},
    ]


def test_redaction_failure_drops_sensitive_fields_but_keeps_hash(monkeypatch):
    def fake_run(cmd, **_kwargs):
        if "scan-pii" in cmd:
            return SimpleNamespace(returncode=1, stdout="", stderr="failed")
        raise AssertionError("record command is invoked separately")

    monkeypatch.setattr(observability_hook.subprocess, "run", fake_run)
    record = _record(_base("UserPromptSubmit", prompt="alice@example.com"))

    redacted = observability_hook._redact_observability_record(record)

    assert "prompt" not in redacted["metrics"]
    assert "user_input" not in redacted["metrics"]
    assert redacted["metrics"]["pii_scan_input_sha256"]


def test_unexpected_processing_error_is_diagnosed_and_fail_open(monkeypatch, capsys):
    def fail_record(_record):
        raise KeyError("alice@example.com")

    monkeypatch.setattr(observability_hook, "_record_observability", fail_record)
    monkeypatch.setattr(
        sys,
        "stdin",
        io.StringIO(json.dumps(_base("UserPromptSubmit", prompt="hello"))),
    )

    observability_hook.main()

    captured = capsys.readouterr()
    assert json.loads(captured.out) == {}
    assert captured.err == (
        "qwen-observability-hook: unexpected KeyError while processing UserPromptSubmit\n"
    )
    assert "alice@example.com" not in captured.err


def test_unexpected_processing_error_does_not_log_untrusted_event_name(
    monkeypatch, capsys
):
    def fail_build(_input_data):
        raise KeyError("secret-value")

    monkeypatch.setattr(observability_hook, "_build_record", fail_build)
    monkeypatch.setattr(
        sys,
        "stdin",
        io.StringIO(json.dumps({"hook_event_name": "alice@example.com"})),
    )

    observability_hook.main()

    captured = capsys.readouterr()
    assert json.loads(captured.out) == {}
    assert captured.err == (
        "qwen-observability-hook: unexpected KeyError while processing unknown\n"
    )
    assert "alice@example.com" not in captured.err
    assert "secret-value" not in captured.err


def test_oversized_payload_is_diagnosed_and_fail_open(monkeypatch, capsys):
    def fail_run(*_args, **_kwargs):
        raise AssertionError("subprocess.run should not be called")

    payload = b"alice@example.com" + b"x" * observability_hook._MAX_PAYLOAD_SIZE
    monkeypatch.setattr(observability_hook.subprocess, "run", fail_run)
    monkeypatch.setattr(sys, "stdin", SimpleNamespace(buffer=io.BytesIO(payload)))

    observability_hook.main()

    captured = capsys.readouterr()
    assert json.loads(captured.out) == {}
    assert captured.err == (
        "qwen-observability-hook: hook input exceeds "
        f"{observability_hook._MAX_PAYLOAD_SIZE} bytes; skipping event\n"
    )
    assert "alice@example.com" not in captured.err


def test_payload_at_size_limit_is_processed(monkeypatch, capsys):
    prefix = (
        b'{"hook_event_name":"UserPromptSubmit",'
        b'"session_id":"session-123","prompt":"'
    )
    suffix = b'"}'
    prompt_size = observability_hook._MAX_PAYLOAD_SIZE - len(prefix) - len(suffix)
    payload = prefix + b"x" * prompt_size + suffix
    records = []
    assert len(payload) == observability_hook._MAX_PAYLOAD_SIZE

    monkeypatch.setattr(observability_hook, "_record_observability", records.append)
    monkeypatch.setattr(sys, "stdin", SimpleNamespace(buffer=io.BytesIO(payload)))

    observability_hook.main()

    captured = capsys.readouterr()
    assert json.loads(captured.out) == {}
    assert captured.err == ""
    assert len(records) == 1
    assert len(records[0]["metrics"]["prompt"]) == prompt_size


def test_invalid_json_returns_noop_without_cli(monkeypatch, capsys):
    def fail_run(*_args, **_kwargs):
        raise AssertionError("subprocess.run should not be called")

    monkeypatch.setattr(observability_hook.subprocess, "run", fail_run)
    monkeypatch.setattr(sys, "stdin", io.StringIO("not-json"))

    observability_hook.main()

    assert json.loads(capsys.readouterr().out) == {}


def test_non_object_json_returns_noop_without_cli(monkeypatch, capsys):
    def fail_run(*_args, **_kwargs):
        raise AssertionError("subprocess.run should not be called")

    monkeypatch.setattr(observability_hook.subprocess, "run", fail_run)
    monkeypatch.setattr(sys, "stdin", io.StringIO("[]"))

    observability_hook.main()

    assert json.loads(capsys.readouterr().out) == {}


def test_manifest_mounts_skill_ledger_code_scanner_observability_and_pii_hooks():
    manifest = json.loads((_EXTENSION_DIR / "qwen-extension.json").read_text())
    expected_events = {
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "PostToolUseFailure",
        "Stop",
        "StopFailure",
    }

    assert set(manifest["hooks"]) == expected_events
    pre_tool_entries = manifest["hooks"]["PreToolUse"]
    assert len(pre_tool_entries) == 3
    skill_group = pre_tool_entries[0]
    assert skill_group["matcher"] == "^skill$"
    assert skill_group["sequential"] is True
    skill_hook = skill_group["hooks"][0]
    assert skill_hook["name"] == "agent-sec-skill-ledger"
    assert "async" not in skill_hook
    assert skill_hook["command"] == (
        'python3 "${extensionPath}${/}hooks${/}skill_ledger_hook.py"'
    )

    scanner_group = pre_tool_entries[1]
    assert scanner_group["matcher"] == "^run_shell_command$"
    scanner_hooks = scanner_group["hooks"]
    assert len(scanner_hooks) == 1
    scanner_hook = scanner_hooks[0]
    assert scanner_hook["name"] == "agent-sec-code-scan-shell-command"
    assert scanner_hook["command"] == (
        'python3 "${extensionPath}${/}hooks${/}code_scanner_hook.py"'
    )
    assert scanner_hook["timeout"] == 10000
    assert "async" not in scanner_hook

    for event_name, entries in manifest["hooks"].items():
        policy_group = entries[-1]
        hooks = policy_group["hooks"]
        hooks_by_name = {hook["name"]: hook for hook in hooks}
        expected_names = {
            "agent-sec-observability",
            "agent-sec-pii-checker",
        }
        if event_name == "UserPromptSubmit":
            expected_names.add("agent-sec-prompt-scanner")
            prompt_scanner = hooks_by_name["agent-sec-prompt-scanner"]
            assert prompt_scanner.get("async") is None
            assert prompt_scanner["command"] == (
                'python3 "${extensionPath}${/}hooks${/}prompt_scanner_hook.py"'
            )
            assert prompt_scanner["timeout"] == 15000

        assert set(hooks_by_name) == expected_names
        observability = hooks_by_name["agent-sec-observability"]
        assert observability["async"] is True
        assert observability["command"] == (
            'python3 "${extensionPath}${/}hooks${/}observability_hook.py"'
        )
        pii_checker = hooks_by_name["agent-sec-pii-checker"]
        assert "async" not in pii_checker
        assert pii_checker["timeout"] == 10000
        assert pii_checker["command"] == (
            'python3 "${extensionPath}${/}hooks${/}pii_checker_hook.py"'
        )
        if event_name != "PreToolUse":
            assert len(entries) == 1
