"""Unit tests for Hermes observability capability."""

from __future__ import annotations

import inspect
from unittest.mock import patch

from hermes_plugin_src.capabilities import ALL_CAPABILITIES
from hermes_plugin_src.capabilities.observability import ObservabilityCapability
from hermes_plugin_src.cli_runner import CliResult
from hermes_plugin_src.pii_text import text_sha256


class InlineThread:
    def __init__(self, target, args=(), kwargs=None, daemon=None, name=None):
        self._target = target
        self._args = args
        self._kwargs = kwargs or {}
        self.daemon = daemon
        self.name = name

    def start(self):
        self._target(*self._args, **self._kwargs)


def _make_capability() -> ObservabilityCapability:
    cap = ObservabilityCapability()
    cap._timeout = 5.0
    cap._on_register({})
    return cap


def test_observability_timeout_env_overrides_config(monkeypatch):
    monkeypatch.setenv("OBSERVABILITY_TIMEOUT", "3")
    cap = _make_capability()

    assert cap._timeout == 3


def test_observability_timeout_is_capped(monkeypatch):
    for value in ("7", "999999"):
        monkeypatch.setenv("OBSERVABILITY_TIMEOUT", value)
        cap = _make_capability()
        assert cap._timeout == 5.0


def test_observability_config_timeout_is_capped(monkeypatch):
    monkeypatch.delenv("OBSERVABILITY_TIMEOUT", raising=False)
    cap = ObservabilityCapability()
    cap._timeout = 7.0
    cap._on_register({})

    assert cap._timeout == 5.0


def test_invalid_observability_timeout_env_uses_config(monkeypatch):
    for value in ("", "invalid", "0", "-1"):
        monkeypatch.setenv("OBSERVABILITY_TIMEOUT", value)
        cap = _make_capability()
        assert cap._timeout == 5.0


def test_get_hooks_define_registers_expected_hooks():
    cap = _make_capability()

    assert set(cap.get_hooks_define()) == {
        "pre_llm_call",
        "pre_api_request",
        "post_api_request",
        "pre_tool_call",
        "post_tool_call",
        "post_llm_call",
    }


def test_hook_handlers_use_explicit_contracts():
    cap = _make_capability()

    for callback in cap.get_hooks_define().values():
        signature = inspect.signature(callback)
        assert inspect.Parameter.VAR_POSITIONAL not in {
            parameter.kind for parameter in signature.parameters.values()
        }

    pre_tool_signature = inspect.signature(cap._on_pre_tool_call)
    assert (
        pre_tool_signature.parameters["tool_name"].kind
        is inspect.Parameter.KEYWORD_ONLY
    )
    assert pre_tool_signature.parameters["args"].kind is inspect.Parameter.KEYWORD_ONLY

    post_tool_signature = inspect.signature(cap._on_post_tool_call)
    assert (
        post_tool_signature.parameters["tool_name"].kind
        is inspect.Parameter.KEYWORD_ONLY
    )
    assert post_tool_signature.parameters["args"].kind is inspect.Parameter.KEYWORD_ONLY
    assert (
        post_tool_signature.parameters["result"].kind is inspect.Parameter.KEYWORD_ONLY
    )

    pre_llm_signature = inspect.signature(cap._on_pre_llm_call)
    assert (
        pre_llm_signature.parameters["messages"].kind
        is inspect.Parameter.POSITIONAL_OR_KEYWORD
    )

    post_llm_signature = inspect.signature(cap._on_post_llm_call)
    assert (
        post_llm_signature.parameters["messages"].kind
        is inspect.Parameter.POSITIONAL_OR_KEYWORD
    )
    assert (
        post_llm_signature.parameters["response"].kind
        is inspect.Parameter.POSITIONAL_OR_KEYWORD
    )


def test_pre_llm_call_records_observability_payload_without_blocking_on_result():
    cap = _make_capability()

    with (
        patch(
            "hermes_plugin_src.capabilities.observability.threading.Thread",
            InlineThread,
        ),
        patch(
            "hermes_plugin_src.capabilities.observability.record_hermes_observability",
            return_value=CliResult(stdout="", stderr="", exit_code=0),
        ) as mock_record,
    ):
        result = cap._on_pre_llm_call(
            session_id="session-1",
            user_message="hello",
            conversation_history=[],
            model="gpt-test",
            platform="hermes",
        )

    assert result is None
    mock_record.assert_called_once()
    payload = mock_record.call_args.args[0]
    assert mock_record.call_args.kwargs["timeout"] == 5.0
    assert payload["hook"] == "before_agent_run"
    assert payload["metadata"]["sessionId"] == "session-1"
    assert payload["metadata"]["runId"] == "00000000-0000-0000-0000-000000000000"


def test_pre_llm_call_accepts_positional_messages():
    cap = _make_capability()

    with (
        patch(
            "hermes_plugin_src.capabilities.observability.threading.Thread",
            InlineThread,
        ),
        patch(
            "hermes_plugin_src.capabilities.observability.record_hermes_observability",
            return_value=CliResult(stdout="", stderr="", exit_code=0),
        ) as mock_record,
    ):
        result = cap._on_pre_llm_call(
            [{"role": "user", "content": "hello"}],
            session_id="session-1",
            model="gpt-test",
        )

    assert result is None
    mock_record.assert_called_once()
    payload = mock_record.call_args.args[0]
    assert payload["hook"] == "before_agent_run"
    assert payload["metrics"] == {
        "prompt": None,
        "user_input": None,
        "pii_scan_input_sha256": text_sha256("hello"),
        "model_id": "gpt-test",
        "model_provider": None,
    }


def test_post_llm_call_accepts_positional_response():
    cap = _make_capability()

    with (
        patch(
            "hermes_plugin_src.capabilities.observability.threading.Thread",
            InlineThread,
        ),
        patch(
            "hermes_plugin_src.capabilities.observability.record_hermes_observability",
            return_value=CliResult(stdout="", stderr="", exit_code=0),
        ) as mock_record,
    ):
        result = cap._on_post_llm_call(
            [{"role": "assistant", "content": "done"}],
            "done",
            session_id="session-1",
        )

    assert result is None
    mock_record.assert_called_once()
    payload = mock_record.call_args.args[0]
    assert payload["hook"] == "after_agent_run"
    assert payload["metrics"] == {
        "response": "done",
        "final_model_id": None,
        "final_model_provider": None,
    }


def test_skips_cli_call_when_record_cannot_be_built():
    cap = _make_capability()

    with (
        patch(
            "hermes_plugin_src.capabilities.observability.threading.Thread",
            InlineThread,
        ),
        patch(
            "hermes_plugin_src.capabilities.observability.record_hermes_observability"
        ) as mock_record,
    ):
        result = cap._on_pre_tool_call(
            tool_name="terminal",
            args={"command": "ls"},
        )

    assert result is None
    mock_record.assert_not_called()


def test_capability_handlers_emit_all_registered_hook_types():
    cap = _make_capability()

    with (
        patch(
            "hermes_plugin_src.capabilities.observability.threading.Thread",
            InlineThread,
        ),
        patch(
            "hermes_plugin_src.capabilities.observability.record_hermes_observability",
            return_value=CliResult(stdout="", stderr="", exit_code=0),
        ) as mock_record,
    ):
        cap._on_pre_llm_call(session_id="session-1", user_message="hello")
        cap._on_pre_api_request(
            session_id="session-1",
            task_id="task-1",
            api_call_count=1,
            model="gpt-test",
        )
        cap._on_post_api_request(
            session_id="session-1",
            task_id="task-1",
            api_call_count=1,
            api_duration=12.0,
        )
        cap._on_pre_tool_call(
            tool_name="terminal",
            args={"command": "ls"},
            session_id="session-1",
            task_id="task-1",
            tool_call_id="tool-1",
        )
        cap._on_post_tool_call(
            tool_name="terminal",
            args={"command": "ls"},
            result={"stdout": "ok", "exit_code": 0},
            session_id="session-1",
            task_id="task-1",
            tool_call_id="tool-1",
            duration_ms=5,
        )
        cap._on_post_llm_call(
            session_id="session-1",
            assistant_response="done",
            model="gpt-test",
            platform="hermes",
        )

    assert [call.args[0]["hook"] for call in mock_record.call_args_list] == [
        "before_agent_run",
        "before_llm_call",
        "after_llm_call",
        "before_tool_call",
        "after_tool_call",
        "after_agent_run",
    ]


def test_post_tool_call_preserves_explicit_none_result():
    cap = _make_capability()

    with (
        patch(
            "hermes_plugin_src.capabilities.observability.threading.Thread",
            InlineThread,
        ),
        patch(
            "hermes_plugin_src.capabilities.observability.record_hermes_observability",
            return_value=CliResult(stdout="", stderr="", exit_code=0),
        ) as mock_record,
    ):
        result = cap._on_post_tool_call(
            tool_name="terminal",
            args={"command": "true"},
            result=None,
            session_id="session-1",
            tool_call_id="tool-1",
        )

    assert result is None
    mock_record.assert_called_once()
    payload = mock_record.call_args.args[0]
    assert payload["hook"] == "after_tool_call"
    assert payload["metrics"] == {
        "result": None,
        "duration_ms": None,
        "exit_code": None,
        "error": None,
    }


def test_observability_is_exported_in_all_capabilities():
    assert "observability" in [cap.id for cap in ALL_CAPABILITIES]


def test_record_logs_cli_failure_details(caplog):
    cap = _make_capability()
    record = {
        "hook": "before_llm_call",
        "metadata": {
            "sessionId": "session-1",
            "runId": "00000000-0000-0000-0000-000000000000",
        },
        "metrics": {"model_id": "gpt-test"},
    }

    with patch(
        "hermes_plugin_src.capabilities.observability.record_hermes_observability",
        return_value=CliResult(
            stdout="validation details",
            stderr="schema validation failed",
            exit_code=2,
        ),
    ):
        with caplog.at_level("WARNING", logger="agent-sec-core"):
            cap._record(record)

    assert "observability record failed" in caplog.text
    assert "hook=before_llm_call" in caplog.text
    assert "exit_code=2" in caplog.text
    assert "stderr=schema validation failed" in caplog.text
    assert "stdout=validation details" in caplog.text


def test_record_logs_unexpected_cli_exception(caplog):
    cap = _make_capability()
    record = {
        "hook": "before_agent_run",
        "metadata": {
            "sessionId": "session-1",
            "runId": "00000000-0000-0000-0000-000000000000",
        },
        "metrics": {"model_id": "gpt-test"},
    }

    with patch(
        "hermes_plugin_src.capabilities.observability.record_hermes_observability",
        side_effect=RuntimeError("spawn failed"),
    ):
        with caplog.at_level("WARNING", logger="agent-sec-core"):
            cap._record(record)

    assert "observability record error" in caplog.text
    assert "hook=before_agent_run" in caplog.text
    assert "RuntimeError: spawn failed" in caplog.text
