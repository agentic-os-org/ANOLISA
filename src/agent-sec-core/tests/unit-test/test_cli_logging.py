"""Unit tests for agent_sec_cli.cli_logging."""

import json
import logging
import os
import re
import stat
import sys
from datetime import datetime, timezone
from pathlib import Path

import pytest
from agent_sec_cli import diagnostic_logging
from agent_sec_cli.cli_logging import (
    _RUST_LOGGER_NAMES,
    CLI_LOG_BACKUP_COUNT,
    CLI_LOG_MAX_BYTES,
    JsonlCliLogHandler,
    _reset_cli_logging_for_tests,
    resolve_cli_logging_config,
    setup_cli_logging,
)
from agent_sec_cli.correlation_context import (
    TraceContext,
    clear_invocation_context_for_tests,
    clear_process_trace_context,
    init_invocation_context,
    init_process_trace_context,
)

_AGENT_SEC_CLI_DIR = Path(__file__).resolve().parents[2] / "agent-sec-cli"
_LOG_MACRO_RE = re.compile(r"\blog::(?:error|warn|info|debug|trace)!")


@pytest.fixture(autouse=True)
def reset_logging_state() -> None:
    clear_process_trace_context()
    clear_invocation_context_for_tests()
    _reset_cli_logging_for_tests()
    yield
    _reset_cli_logging_for_tests()
    clear_invocation_context_for_tests()
    clear_process_trace_context()


def _clear_cli_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("AGENT_SEC_CLI_LOG_LEVEL", raising=False)
    monkeypatch.delenv("AGENT_SEC_INVOCATION_ID", raising=False)


def _make_record(
    level: int = logging.WARNING,
    msg: str = "action completed",
    exc_info: tuple | None = None,
) -> logging.LogRecord:
    return logging.LogRecord(
        name="agent_sec_cli.tests",
        level=level,
        pathname=__file__,
        lineno=1,
        msg=msg,
        args=(),
        exc_info=exc_info,
        func="test_function",
    )


def test_default_config_uses_cli_stream_path(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _clear_cli_env(monkeypatch)
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path))

    config = resolve_cli_logging_config()

    assert config.enabled is True
    assert config.level == logging.WARNING
    assert config.log_file == tmp_path / "cli.jsonl"


@pytest.mark.parametrize(
    ("value", "level"),
    [
        ("debug", logging.DEBUG),
        ("info", logging.INFO),
        ("warning", logging.WARNING),
        ("error", logging.ERROR),
        ("critical", logging.CRITICAL),
    ],
)
def test_log_level_env_accepts_supported_levels(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, value: str, level: int
) -> None:
    _clear_cli_env(monkeypatch)
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path))
    monkeypatch.setenv("AGENT_SEC_CLI_LOG_LEVEL", value)

    config = resolve_cli_logging_config()

    assert config.enabled is True
    assert config.level == level


def test_log_level_off_disables_cli_logging(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _clear_cli_env(monkeypatch)
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path))
    monkeypatch.setenv("AGENT_SEC_CLI_LOG_LEVEL", "off")

    config = resolve_cli_logging_config()

    assert config.enabled is False
    assert config.log_file is None


def test_invalid_log_level_falls_back_to_warning(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _clear_cli_env(monkeypatch)
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path))
    monkeypatch.setenv("AGENT_SEC_CLI_LOG_LEVEL", "verbose")

    config = resolve_cli_logging_config()

    assert config.enabled is True
    assert config.level == logging.WARNING


def test_data_dir_override_resolves_cli_log_under_it(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # cli.jsonl follows AGENT_SEC_DATA_DIR (the shared knob for all three
    # streams) — there is no per-file override env. The shared helper
    # _resolve_data_dir() forces the directory to 0o700.
    _clear_cli_env(monkeypatch)
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path))

    config = resolve_cli_logging_config()

    assert config.enabled is True
    assert config.log_file == tmp_path / "cli.jsonl"
    assert stat.S_IMODE(tmp_path.stat().st_mode) == 0o700


def test_default_path_resolution_failure_disables_cli_logging(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _clear_cli_env(monkeypatch)
    invalid_data_dir = tmp_path / "not-a-directory"
    invalid_data_dir.write_text("not a directory", encoding="utf-8")
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(invalid_data_dir))

    config = resolve_cli_logging_config()

    assert config.enabled is False
    assert config.log_file is None


def test_handler_uses_configured_retention_constants(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    created_writers: list[tuple[Path, int, int]] = []

    class CapturingWriter:
        def __init__(
            self,
            path: str | Path,
            *,
            max_bytes: int,
            backup_count: int,
        ) -> None:
            created_writers.append((Path(path), max_bytes, backup_count))

        def write(self, _record: dict[str, object]) -> None:
            pass

    monkeypatch.setattr(diagnostic_logging, "JsonlEventWriter", CapturingWriter)

    JsonlCliLogHandler(tmp_path / "cli.jsonl")

    assert created_writers == [
        (tmp_path / "cli.jsonl", CLI_LOG_MAX_BYTES, CLI_LOG_BACKUP_COUNT)
    ]


def test_handler_writes_jsonl_with_invocation_and_trace_context(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _clear_cli_env(monkeypatch)
    monkeypatch.setenv("AGENT_SEC_INVOCATION_ID", "invocation-1")
    init_invocation_context()
    init_process_trace_context(
        TraceContext(
            trace_id="trace-1",
            session_id="session-1",
            run_id="run-1",
            call_id="call-1",
            tool_call_id="tool-1",
        )
    )
    path = tmp_path / "cli.jsonl"
    handler = JsonlCliLogHandler(path)
    record = _make_record()
    # New schema: domain-specific fields live inside `data`.
    record.data = {
        "action": "code_scan",
        "caller": "cli",
        "exit_code": 1,
        "duration_ms": 12.5,
    }

    handler.emit(record)

    payload = json.loads(path.read_text(encoding="utf-8").splitlines()[0])
    assert payload["level"] == "WARNING"
    assert payload["component"] == "cli"
    assert payload["event"] == "cli_log"
    assert payload["pid"] == os.getpid()
    assert payload["logger"] == "agent_sec_cli.tests"
    assert payload["message"] == "action completed"
    assert payload["function"] == "test_function"
    assert payload["invocation_id"] == "invocation-1"
    assert payload["trace_id"] == "trace-1"
    assert payload["session_id"] == "session-1"
    assert payload["run_id"] == "run-1"
    assert payload["call_id"] == "call-1"
    assert payload["tool_call_id"] == "tool-1"
    assert payload["data"] == {
        "action": "code_scan",
        "caller": "cli",
        "exit_code": 1,
        "duration_ms": 12.5,
    }
    # Domain-specific keys never leak to top level under the new schema —
    # they go through `data`.
    for key in ("action", "caller", "exit_code", "duration_ms", "module"):
        assert key not in payload


def test_handler_forwards_string_data_payload(tmp_path: Path) -> None:
    init_invocation_context()
    path = tmp_path / "cli.jsonl"
    handler = JsonlCliLogHandler(path)
    record = _make_record()
    record.data = "retry budget exhausted after 5 attempts"

    handler.emit(record)

    payload = json.loads(path.read_text(encoding="utf-8").splitlines()[0])
    assert payload["data"] == "retry budget exhausted after 5 attempts"


def test_handler_does_not_leak_unrelated_record_attributes(tmp_path: Path) -> None:
    # The handler reads only well-known slots (correlation IDs + data + exc_info).
    # Arbitrary extras attached to a record do not appear in the output —
    # this protects against accidental leakage of caller-side state.
    init_invocation_context()
    path = tmp_path / "cli.jsonl"
    handler = JsonlCliLogHandler(path)
    record = _make_record()
    record.metadata = {"secret": "should-not-leak"}
    record.tenant_id = "tenant-42"

    handler.emit(record)

    payload = json.loads(path.read_text(encoding="utf-8").splitlines()[0])
    assert "metadata" not in payload
    assert "tenant_id" not in payload
    assert "data" not in payload  # nothing in data either


def test_handler_uses_record_trace_id_when_process_trace_context_is_empty(
    tmp_path: Path,
) -> None:
    init_invocation_context()
    path = tmp_path / "cli.jsonl"
    handler = JsonlCliLogHandler(path)
    record = _make_record()
    record.trace_id = "generated-request-trace"

    handler.emit(record)

    payload = json.loads(path.read_text(encoding="utf-8").splitlines()[0])
    assert payload["trace_id"] == "generated-request-trace"


def test_handler_records_error_exception_metadata(tmp_path: Path) -> None:
    init_invocation_context()
    path = tmp_path / "cli.jsonl"
    handler = JsonlCliLogHandler(path)

    try:
        raise ValueError("bad value")
    except ValueError:
        record = _make_record(
            level=logging.ERROR,
            msg="backend raised an exception",
            exc_info=sys.exc_info(),
        )
        handler.emit(record)

    payload = json.loads(path.read_text(encoding="utf-8").splitlines()[0])
    assert payload["level"] == "ERROR"
    assert payload["error_type"] == "ValueError"
    assert payload["exception"] == "bad value"
    assert "ValueError: bad value" in payload["traceback"]


def test_handler_ignores_empty_exc_info_tuple(tmp_path: Path) -> None:
    init_invocation_context()
    path = tmp_path / "cli.jsonl"
    handler = JsonlCliLogHandler(path)
    record = _make_record(
        level=logging.ERROR,
        msg="error called outside an active exception",
        exc_info=(None, None, None),
    )

    handler.emit(record)

    payload = json.loads(path.read_text(encoding="utf-8").splitlines()[0])
    assert payload["level"] == "ERROR"
    assert "error_type" not in payload
    assert "exception" not in payload
    assert "traceback" not in payload


def test_handler_omits_traceback_for_warning_with_exception_info(
    tmp_path: Path,
) -> None:
    init_invocation_context()
    path = tmp_path / "cli.jsonl"
    handler = JsonlCliLogHandler(path)

    try:
        raise ValueError("warning value")
    except ValueError:
        record = _make_record(
            level=logging.WARNING,
            msg="backend returned warning with context",
            exc_info=sys.exc_info(),
        )
        handler.emit(record)

    payload = json.loads(path.read_text(encoding="utf-8").splitlines()[0])
    assert payload["level"] == "WARNING"
    assert payload["error_type"] == "ValueError"
    assert payload["exception"] == "warning value"
    assert "traceback" not in payload


def test_handler_stringifies_non_json_data_values(tmp_path: Path) -> None:
    init_invocation_context()
    path = tmp_path / "cli.jsonl"
    handler = JsonlCliLogHandler(path)
    record = _make_record()
    observed_at = datetime(2026, 6, 4, tzinfo=timezone.utc)
    record.data = {
        "path": Path("/tmp/x"),
        "observed_at": observed_at,
        "nested": {"items": [Path("relative")]},
    }

    handler.emit(record)

    payload = json.loads(path.read_text(encoding="utf-8").splitlines()[0])
    assert payload["data"] == {
        "path": "/tmp/x",
        "observed_at": str(observed_at),
        "nested": {"items": ["relative"]},
    }


def test_handler_tightens_existing_log_file_on_emit(tmp_path: Path) -> None:
    init_invocation_context()
    path = tmp_path / "cli.jsonl"
    path.write_text("", encoding="utf-8")
    path.chmod(0o644)
    handler = JsonlCliLogHandler(path)

    handler.emit(_make_record())

    assert stat.S_IMODE(path.stat().st_mode) == 0o600


def test_setup_cli_logging_is_idempotent(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _clear_cli_env(monkeypatch)
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path))

    setup_cli_logging()
    setup_cli_logging()

    logger = logging.getLogger("agent_sec_cli")
    handlers = [
        handler
        for handler in logger.handlers
        if isinstance(handler, JsonlCliLogHandler)
    ]
    assert len(handlers) == 1


def test_setup_cli_logging_disables_root_propagation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _clear_cli_env(monkeypatch)
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path))
    logger = logging.getLogger("agent_sec_cli")
    logger.propagate = True

    setup_cli_logging()

    assert logger.propagate is False


def test_setup_cli_logging_does_not_mutate_cli_logger_level(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _clear_cli_env(monkeypatch)
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path))
    monkeypatch.setenv("AGENT_SEC_CLI_LOG_LEVEL", "debug")
    logger = logging.getLogger("agent_sec_cli")
    original_level = logger.level

    try:
        logger.setLevel(logging.ERROR)
        setup_cli_logging()

        assert logger.level == logging.ERROR
    finally:
        logger.setLevel(original_level)


def test_setup_cli_logging_preserves_root_propagation_when_logging_is_disabled(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _clear_cli_env(monkeypatch)
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path))
    monkeypatch.setenv("AGENT_SEC_CLI_LOG_LEVEL", "off")
    logger = logging.getLogger("agent_sec_cli")
    logger.propagate = True

    setup_cli_logging()

    handlers = [
        handler
        for handler in logger.handlers
        if isinstance(handler, JsonlCliLogHandler)
    ]
    assert handlers == []
    assert logger.propagate is True


def test_setup_cli_logging_handler_failure_is_sticky_no_retry(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # A setup failure marks the call as done — subsequent calls are no-ops
    # even if the underlying cause would now succeed. Production never
    # reconfigures mid-process, so failure is sticky and silent.
    _clear_cli_env(monkeypatch)
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path))

    class RaisingHandler:
        def __init__(self, _path: Path) -> None:
            raise RuntimeError("handler boom")

    monkeypatch.setattr("agent_sec_cli.cli_logging.JsonlCliLogHandler", RaisingHandler)

    setup_cli_logging()

    # Restore the real handler — but the second setup should still be a no-op.
    monkeypatch.setattr(
        "agent_sec_cli.cli_logging.JsonlCliLogHandler",
        JsonlCliLogHandler,
    )
    setup_cli_logging()

    logger = logging.getLogger("agent_sec_cli")
    handlers = [
        handler
        for handler in logger.handlers
        if isinstance(handler, JsonlCliLogHandler)
    ]
    assert handlers == []
    assert logger.propagate is True


def test_handler_ignores_non_string_correlation_field_overrides(tmp_path: Path) -> None:
    """A misuse like ``extra={"trace_id": trace_ctx_object}`` must NOT
    silently overwrite the real string trace_id with a repr — a downstream
    ``WHERE trace_id = ?`` query would fail with that. Non-string values are
    skipped, leaving the process-level value intact. See PR #651 review #13.
    """
    init_invocation_context()
    init_process_trace_context(
        TraceContext(trace_id="real-trace", session_id="real-session")
    )
    path = tmp_path / "cli.jsonl"
    handler = JsonlCliLogHandler(path)
    record = _make_record()
    # Caller mistakenly passes the whole context object instead of a string id.
    record.trace_id = TraceContext(trace_id="ignored")
    record.session_id = 12345  # int instead of str
    record.run_id = "real-run"  # this one is fine

    handler.emit(record)

    payload = json.loads(path.read_text(encoding="utf-8").splitlines()[0])
    # Process-level value preserved — record-level non-string did not overwrite.
    assert payload["trace_id"] == "real-trace"
    assert payload["session_id"] == "real-session"
    assert payload["run_id"] == "real-run"


def test_setup_cli_logging_collects_rust_bridge_records(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # pyo3-log names each logger after the emitting Rust crate's `log` target,
    # so these records never pass through the `agent_sec_cli` tree. They still
    # belong to the CLI stream: they describe this invocation's scan.
    _clear_cli_env(monkeypatch)
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path))
    monkeypatch.setenv("AGENT_SEC_INVOCATION_ID", "invocation-rust")
    init_invocation_context()

    setup_cli_logging()
    logging.getLogger("model_service").warning(
        "Ignoring AGENT_SEC_MODEL_SERVICE_TIMEOUT"
    )
    logging.getLogger("prompt_scanner.scanner").warning("Detector is not available")

    payloads = [
        json.loads(line)
        for line in (tmp_path / "cli.jsonl").read_text(encoding="utf-8").splitlines()
    ]
    assert [payload["logger"] for payload in payloads] == [
        "model_service",
        "prompt_scanner.scanner",
    ]
    assert {payload["component"] for payload in payloads} == {"cli"}
    assert {payload["event"] for payload in payloads} == {"cli_log"}
    # Correlation still works: Rust records join the rest of the invocation.
    assert {payload["invocation_id"] for payload in payloads} == {"invocation-rust"}


def test_setup_cli_logging_shares_one_handler_across_rust_loggers(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # A single handler instance keeps one stream and one level authoritative;
    # disabling propagation stops the records from also reaching the
    # handler-less root, where logging.lastResort would print raw text to
    # stderr that hook layers capture and discard.
    _clear_cli_env(monkeypatch)
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path))

    setup_cli_logging()

    attached = set()
    for name in ("agent_sec_cli", *_RUST_LOGGER_NAMES):
        logger = logging.getLogger(name)
        handlers = [
            handler
            for handler in logger.handlers
            if isinstance(handler, JsonlCliLogHandler)
        ]
        assert len(handlers) == 1
        assert logger.propagate is False
        attached.add(id(handlers[0]))
    assert len(attached) == 1


def test_reset_detaches_handlers_from_rust_loggers(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # A handler left behind on a Rust logger would keep writing to the previous
    # test's log path for the rest of the process.
    _clear_cli_env(monkeypatch)
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path))
    setup_cli_logging()

    _reset_cli_logging_for_tests()

    for name in ("agent_sec_cli", *_RUST_LOGGER_NAMES):
        logger = logging.getLogger(name)
        assert [
            handler
            for handler in logger.handlers
            if isinstance(handler, JsonlCliLogHandler)
        ] == []
        assert logger.propagate is True


def test_logging_rust_crates_are_covered_by_cli_logger_names() -> None:
    # The set of bridged logger names is maintained by hand, so a new crate
    # that starts logging would silently fall out of the CLI stream again.
    # Fail here instead of losing its records in production.
    crates_dir = _AGENT_SEC_CLI_DIR / "crates"
    if not crates_dir.is_dir():
        pytest.skip("Rust sources are unavailable outside a source checkout")

    logging_crates = {
        source.relative_to(crates_dir).parts[0].replace("-", "_")
        for source in crates_dir.glob("*/src/**/*.rs")
        if _LOG_MACRO_RE.search(source.read_text(encoding="utf-8"))
    }

    assert logging_crates, "expected at least one Rust crate to emit log records"
    assert logging_crates <= set(_RUST_LOGGER_NAMES), (
        "Rust crates emitting log records but missing from _RUST_LOGGER_NAMES: "
        f"{sorted(logging_crates - set(_RUST_LOGGER_NAMES))}"
    )


def test_native_extension_crate_emits_no_log_records() -> None:
    # Records from the cdylib itself would arrive under `agent_sec_cli_native`
    # (its Cargo `[lib] name`) — a top-level logger that neither the
    # `agent_sec_cli` tree nor _RUST_LOGGER_NAMES covers.
    native_src_dir = _AGENT_SEC_CLI_DIR / "src"
    if not native_src_dir.is_dir():
        pytest.skip("Rust sources are unavailable outside a source checkout")

    offenders = sorted(
        source.name
        for source in native_src_dir.glob("*.rs")
        if _LOG_MACRO_RE.search(source.read_text(encoding="utf-8"))
    )

    assert offenders == [], (
        "log records emitted from the cdylib crate would bypass the CLI "
        f"diagnostic stream: {offenders}"
    )
