"""Unit tests for shared diagnostic JSONL logging."""

import json
import logging
import os
import sys
from pathlib import Path

from agent_sec_cli.correlation_context import (
    TraceContext,
    reset_current_trace_context,
    set_current_trace_context,
)
from agent_sec_cli.diagnostic_logging import (
    BaseDiagnosticLogging,
    JsonlDiagnosticLogger,
    PythonLogRecordDiagnosticLogging,
    record_correlation_overrides,
    resolve_log_level,
)


def test_resolve_log_level_accepts_supported_values() -> None:
    assert resolve_log_level("debug") == (True, logging.DEBUG)
    assert resolve_log_level("INFO") == (True, logging.INFO)
    assert resolve_log_level("off") == (False, logging.WARNING)
    assert resolve_log_level("unknown") == (True, logging.WARNING)
    assert resolve_log_level(None, default=logging.INFO) == (True, logging.INFO)


def test_diagnostic_logger_writes_unified_jsonl_envelope(tmp_path: Path) -> None:
    path = tmp_path / "diagnostic.jsonl"
    logger = JsonlDiagnosticLogger(path=path, level=logging.INFO)

    logger.write(
        level=logging.INFO,
        component="cli",
        event="unit_event",
        message="action completed",
        logger_name="agent_sec_cli.tests",
        function="test_function",
        invocation_id="invocation-1",
        request_id="request-1",
        trace_context=TraceContext(
            trace_id="trace-1",
            session_id="session-1",
            run_id="run-1",
        ),
        correlation_overrides={
            "trace_id": "trace-override",
            "session_id": 12345,
            "call_id": "call-1",
        },
        data={"path": path, "items": [Path("relative")]},
    )

    payload = json.loads(path.read_text(encoding="utf-8").splitlines()[0])
    assert payload["level"] == "INFO"
    assert payload["component"] == "cli"
    assert payload["event"] == "unit_event"
    assert payload["message"] == "action completed"
    assert payload["logger"] == "agent_sec_cli.tests"
    assert payload["function"] == "test_function"
    assert payload["pid"] == os.getpid()
    assert payload["invocation_id"] == "invocation-1"
    assert payload["request_id"] == "request-1"
    assert payload["trace_id"] == "trace-override"
    assert payload["session_id"] == "session-1"
    assert payload["run_id"] == "run-1"
    assert payload["call_id"] == "call-1"
    assert payload["data"] == {"path": str(path), "items": ["relative"]}


def test_diagnostic_logger_filters_below_configured_level(tmp_path: Path) -> None:
    path = tmp_path / "diagnostic.jsonl"
    logger = JsonlDiagnosticLogger(path=path, level=logging.WARNING)

    logger.write(
        level=logging.INFO,
        component="cli",
        event="unit_event",
        message="below level",
    )

    assert not path.exists()


def test_diagnostic_logger_records_error_traceback(tmp_path: Path) -> None:
    path = tmp_path / "diagnostic.jsonl"
    logger = JsonlDiagnosticLogger(path=path, level=logging.INFO)

    try:
        raise ValueError("bad value")
    except ValueError:
        logger.write(
            level=logging.ERROR,
            component="cli",
            event="unit_error",
            message="backend failed",
            exc_info=sys.exc_info(),
        )

    payload = json.loads(path.read_text(encoding="utf-8").splitlines()[0])
    assert payload["error_type"] == "ValueError"
    assert payload["exception"] == "bad value"
    assert "ValueError: bad value" in payload["traceback"]


def test_diagnostic_logger_swallows_writer_failures() -> None:
    class RaisingWriter:
        def write(self, _record):
            raise RuntimeError("writer failed")

    logger = JsonlDiagnosticLogger(writer=RaisingWriter(), level=logging.INFO)

    logger.write(
        level=logging.INFO,
        component="cli",
        event="unit_event",
        message="should not raise",
    )


def test_record_correlation_overrides_filters_none_fields() -> None:
    record = logging.LogRecord(
        name="agent_sec_cli.tests",
        level=logging.INFO,
        pathname=__file__,
        lineno=1,
        msg="record event",
        args=(),
        exc_info=None,
    )
    record.trace_id = "trace-1"
    record.session_id = None
    record.run_id = "run-1"

    assert record_correlation_overrides(record) == {
        "trace_id": "trace-1",
        "run_id": "run-1",
    }


def test_base_diagnostic_logging_resolves_env_and_stream_path(
    tmp_path: Path,
    monkeypatch,
) -> None:
    class UnitDiagnosticLogging(BaseDiagnosticLogging):
        component = "unit"
        stream = "unit"
        level_env = "UNIT_LOG_LEVEL"
        default_level = logging.INFO

    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path))
    monkeypatch.setenv("UNIT_LOG_LEVEL", "debug")

    config = UnitDiagnosticLogging().resolve_config()

    assert config.enabled is True
    assert config.level == logging.DEBUG
    assert config.log_file == tmp_path / "unit.jsonl"

    monkeypatch.setenv("UNIT_LOG_LEVEL", "off")
    disabled = UnitDiagnosticLogging().resolve_config()

    assert disabled.enabled is False
    assert disabled.log_file is None


def test_python_log_record_diagnostic_logging_writes_shared_schema(
    tmp_path: Path,
) -> None:
    class UnitPythonLogging(PythonLogRecordDiagnosticLogging):
        component = "unit"
        stream = "unit"
        python_logger_name = "agent_sec_cli.tests.diagnostic_logging"
        event = "unit_log"
        default_level = logging.INFO
        propagate_on_enable = False
        propagate_on_reset = True

    path = tmp_path / "unit.jsonl"
    diagnostic_logging = UnitPythonLogging()
    diagnostic_logging.reset_for_tests()
    logger = logging.getLogger("agent_sec_cli.tests.diagnostic_logging")
    original_level = logger.level

    try:
        logger.setLevel(logging.INFO)
        diagnostic_logging.setup(path=path)
        logger.info(
            "record event",
            extra={
                "data": {"kind": "unit"},
                "trace_id": "trace-from-record",
            },
        )
    finally:
        diagnostic_logging.reset_for_tests()
        logger.setLevel(original_level)

    payload = json.loads(path.read_text(encoding="utf-8").splitlines()[0])
    assert payload["component"] == "unit"
    assert payload["event"] == "unit_log"
    assert payload["message"] == "record event"
    assert payload["logger"] == "agent_sec_cli.tests.diagnostic_logging"
    assert payload["trace_id"] == "trace-from-record"
    assert payload["data"] == {"kind": "unit"}


def test_python_log_record_diagnostic_logging_prefers_explicit_trace_context(
    tmp_path: Path,
) -> None:
    class UnitPythonLogging(PythonLogRecordDiagnosticLogging):
        component = "unit"
        stream = "unit"
        python_logger_name = "agent_sec_cli.tests.diagnostic_logging.priority"
        event = "unit_log"
        default_level = logging.INFO
        propagate_on_enable = False
        propagate_on_reset = True

    path = tmp_path / "unit.jsonl"
    diagnostic_logging = UnitPythonLogging()
    diagnostic_logging.reset_for_tests()
    logger = logging.getLogger("agent_sec_cli.tests.diagnostic_logging.priority")
    original_level = logger.level
    token = set_current_trace_context(
        TraceContext(
            trace_id="ambient-trace",
            session_id="ambient-session",
            run_id="ambient-run",
        )
    )

    try:
        logger.setLevel(logging.INFO)
        diagnostic_logging.setup(path=path)
        logger.info(
            "record event",
            extra={
                "trace_context": TraceContext(
                    trace_id="explicit-trace",
                    call_id="explicit-call",
                ),
            },
        )
    finally:
        diagnostic_logging.reset_for_tests()
        logger.setLevel(original_level)
        reset_current_trace_context(token)

    payload = json.loads(path.read_text(encoding="utf-8").splitlines()[0])
    assert payload["trace_id"] == "explicit-trace"
    assert payload["call_id"] == "explicit-call"
    assert "session_id" not in payload
    assert "run_id" not in payload
