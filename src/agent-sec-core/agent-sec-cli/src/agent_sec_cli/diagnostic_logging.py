"""Shared JSONL diagnostic logging primitives."""

import logging
import os
import traceback as traceback_module
from collections.abc import Mapping
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from agent_sec_cli.correlation_context import (
    CORRELATION_FIELD_NAMES,
    TraceContext,
    clean_correlation_value,
    get_current_trace_context,
    trace_context_to_payload,
)
from agent_sec_cli.security_events.config import get_stream_log_path
from agent_sec_cli.security_events.writer import (
    DEFAULT_BACKUP_COUNT,
    DEFAULT_MAX_BYTES,
    JsonlEventWriter,
)

LOG_LEVELS: dict[str, int] = {
    "debug": logging.DEBUG,
    "info": logging.INFO,
    "warning": logging.WARNING,
    "error": logging.ERROR,
    "critical": logging.CRITICAL,
}


@dataclass(frozen=True)
class DiagnosticLoggingConfig:
    """Resolved diagnostic logging configuration for one component."""

    enabled: bool
    level: int
    log_file: Path | None


def utc_now_iso() -> str:
    """Return the current UTC time in the diagnostic JSONL timestamp format."""
    return (
        datetime.now(timezone.utc)
        .isoformat(timespec="milliseconds")
        .replace("+00:00", "Z")
    )


def json_safe(value: Any) -> Any:
    """Return a JSON-serializable representation for diagnostic payloads."""
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    if isinstance(value, Mapping):
        return {str(key): json_safe(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [json_safe(item) for item in value]
    return str(value)


def resolve_log_level(
    value: str | None, default: int = logging.WARNING
) -> tuple[bool, int]:
    """Resolve a user-facing log level string.

    Returns ``(enabled, level)`` so component-specific config can share level
    parsing while keeping its own env var names and default policy.
    """
    if value is None:
        return True, default

    normalized = value.strip().lower()
    if normalized == "off":
        return False, default
    return True, LOG_LEVELS.get(normalized, default)


def _has_exception(exc_info: object) -> bool:
    if not isinstance(exc_info, tuple) or len(exc_info) != 3:
        return False
    exc_type, exception, _traceback = exc_info
    return exc_type is not None and exception is not None


def record_correlation_overrides(record: logging.LogRecord) -> dict[str, Any]:
    """Return caller-supplied correlation fields from a logging record."""
    overrides: dict[str, Any] = {}
    for field_name in CORRELATION_FIELD_NAMES:
        value = getattr(record, field_name, None)
        if value is not None:
            overrides[field_name] = value
    return overrides


class JsonlDiagnosticLogger:
    """Write structured diagnostic JSONL records through ``JsonlEventWriter``."""

    def __init__(
        self,
        path: str | Path | None = None,
        *,
        level: int = logging.WARNING,
        max_bytes: int = DEFAULT_MAX_BYTES,
        backup_count: int = DEFAULT_BACKUP_COUNT,
        writer: JsonlEventWriter | None = None,
    ) -> None:
        if path is None and writer is None:
            raise ValueError("path or writer is required")

        self.level = level
        self._path = None if path is None else Path(path).expanduser()
        self._writer = (
            writer
            if writer is not None
            else JsonlEventWriter(
                self._path,
                max_bytes=max_bytes,
                backup_count=backup_count,
            )
        )

    def write(
        self,
        *,
        level: int,
        component: str,
        event: str,
        message: str = "",
        logger_name: str | None = None,
        function: str | None = None,
        invocation_id: str | None = None,
        request_id: str | None = None,
        trace_context: TraceContext | None = None,
        correlation_overrides: Mapping[str, Any] | None = None,
        data: Any = None,
        exc_info: object = None,
    ) -> None:
        """Build and write one diagnostic record. Failures are swallowed."""
        if level < self.level:
            return

        try:
            self._writer.write(
                self.build_payload(
                    level=level,
                    component=component,
                    event=event,
                    message=message,
                    logger_name=logger_name,
                    function=function,
                    invocation_id=invocation_id,
                    request_id=request_id,
                    trace_context=trace_context,
                    correlation_overrides=correlation_overrides,
                    data=data,
                    exc_info=exc_info,
                )
            )
        except Exception:  # noqa: BLE001 - diagnostic logging is best-effort
            pass

    def build_payload(
        self,
        *,
        level: int,
        component: str,
        event: str,
        message: str = "",
        logger_name: str | None = None,
        function: str | None = None,
        invocation_id: str | None = None,
        request_id: str | None = None,
        trace_context: TraceContext | None = None,
        correlation_overrides: Mapping[str, Any] | None = None,
        data: Any = None,
        exc_info: object = None,
    ) -> dict[str, Any]:
        """Return the normalized diagnostic log envelope."""
        payload: dict[str, Any] = {
            "timestamp": utc_now_iso(),
            "level": logging.getLevelName(level),
            "component": component,
            "event": event,
            "message": message,
            "pid": os.getpid(),
        }
        if logger_name is not None:
            payload["logger"] = logger_name
        if function is not None:
            payload["function"] = function
        if invocation_id is not None:
            payload["invocation_id"] = invocation_id
        if request_id is not None:
            payload["request_id"] = request_id

        payload.update(trace_context_to_payload(trace_context))
        if correlation_overrides is not None:
            for field_name in CORRELATION_FIELD_NAMES:
                value = clean_correlation_value(
                    field_name,
                    correlation_overrides.get(field_name),
                )
                if value is not None:
                    payload[field_name] = value

        if data is not None:
            payload["data"] = json_safe(data)

        if _has_exception(exc_info):
            exception = exc_info[1]
            payload["error_type"] = type(exception).__name__
            payload["exception"] = str(exception)
            if level >= logging.ERROR:
                payload["traceback"] = "".join(
                    traceback_module.format_exception(*exc_info)
                )
        return json_safe(payload)


class BaseDiagnosticLogging:
    """Common setup behavior for component diagnostic logs."""

    component = ""
    stream = ""
    level_env: str | None = None
    default_level = logging.WARNING

    def __init__(self) -> None:
        self._setup_done = False

    def resolve_config(
        self,
        path: str | Path | None = None,
    ) -> DiagnosticLoggingConfig:
        """Resolve path, enabled state, and level for this component."""
        enabled = True
        resolved_level = self.default_level
        if self.level_env is not None:
            enabled, resolved_level = resolve_log_level(
                os.environ.get(self.level_env),
                default=self.default_level,
            )

        if not enabled:
            return DiagnosticLoggingConfig(
                enabled=False,
                level=resolved_level,
                log_file=None,
            )

        try:
            log_file = (
                Path(path).expanduser() if path is not None else self.resolve_log_path()
            )
        except Exception:  # noqa: BLE001 - diagnostic logging is best-effort
            log_file = None

        if log_file is None:
            return DiagnosticLoggingConfig(
                enabled=False,
                level=resolved_level,
                log_file=None,
            )
        return DiagnosticLoggingConfig(
            enabled=True,
            level=resolved_level,
            log_file=log_file,
        )

    def resolve_log_path(self) -> Path | None:
        """Resolve this component's diagnostic stream path."""
        if not self.stream:
            return None
        try:
            return Path(get_stream_log_path(self.stream)).expanduser()
        except Exception:  # noqa: BLE001 - diagnostic logging is best-effort
            return None

    def setup(
        self,
        path: str | Path | None = None,
    ) -> None:
        """Idempotently configure this component's diagnostic logger."""
        if self._setup_done:
            return

        try:
            config = self.resolve_config(path=path)
            if config.enabled and config.log_file is not None:
                self._setup_enabled(config)
        except Exception:  # noqa: BLE001 - diagnostic logging is best-effort
            pass
        finally:
            self._setup_done = True

    def _setup_enabled(self, config: DiagnosticLoggingConfig) -> None:
        pass

    def reset_for_tests(self) -> None:
        """Reset in-process setup state for tests."""
        self._setup_done = False


class PythonLogRecordDiagnosticLogging(BaseDiagnosticLogging):
    """Base class for components that route Python logging into JSONL."""

    python_logger_name = ""
    # Logger trees outside `python_logger_name` whose records still belong to
    # this component's stream. Needed because a record's logger name is not
    # always chosen by this codebase: pyo3-log maps Rust `log` targets onto
    # top-level Python logger names that no component namespace covers.
    extra_logger_names: tuple[str, ...] = ()
    event = "python_log"
    propagate_on_enable: bool | None = None
    propagate_on_reset: bool | None = None
    max_bytes = DEFAULT_MAX_BYTES
    backup_count = DEFAULT_BACKUP_COUNT

    def __init__(self) -> None:
        super().__init__()
        self._handler: logging.Handler | None = None

    def _setup_enabled(self, config: DiagnosticLoggingConfig) -> None:
        handler = self.create_record_handler(config.log_file)
        handler.setLevel(config.level)
        # One handler instance serves every tree so that a single stream, and a
        # single level, stay authoritative for the component.
        for python_logger in self.get_python_loggers():
            # Keep the component logger's level under application control. Changing
            # it here would affect every other handler attached to the same logger
            # tree, not just this diagnostic JSONL handler.
            python_logger.addHandler(handler)
            if self.propagate_on_enable is not None:
                python_logger.propagate = self.propagate_on_enable
        self._handler = handler

    def get_python_logger(self) -> logging.Logger:
        """Return the Python logger tree this diagnostic logger owns."""
        return logging.getLogger(self.python_logger_name)

    def get_python_loggers(self) -> tuple[logging.Logger, ...]:
        """Return every logger tree this diagnostic logger attaches to.

        The owned tree comes first, followed by the `extra_logger_names` trees.
        """
        return (
            self.get_python_logger(),
            *(logging.getLogger(name) for name in self.extra_logger_names),
        )

    def create_record_handler(self, path: str | Path) -> "DiagnosticLogRecordHandler":
        """Create the handler that converts LogRecord objects into JSONL."""
        diagnostic_logger = self.create_jsonl_logger(
            path=path,
            level=logging.NOTSET,
        )
        return DiagnosticLogRecordHandler(self, diagnostic_logger)

    def create_jsonl_logger(
        self,
        path: str | Path,
        *,
        level: int,
    ) -> JsonlDiagnosticLogger:
        """Create the low-level JSONL logger for this component."""
        return JsonlDiagnosticLogger(
            path=path,
            level=level,
            max_bytes=self.max_bytes,
            backup_count=self.backup_count,
        )

    def write_log_record(
        self,
        record: logging.LogRecord,
        diagnostic_logger: JsonlDiagnosticLogger,
    ) -> None:
        """Write one LogRecord through the provided diagnostic logger."""
        diagnostic_logger.write(
            level=record.levelno,
            component=self.component,
            event=self.event_for_record(record),
            message=self.message_for_record(record),
            logger_name=record.name,
            function=record.funcName,
            invocation_id=self.invocation_id_for_record(record),
            request_id=self.request_id_for_record(record),
            trace_context=self.trace_context_for_record(record),
            correlation_overrides=self.correlation_overrides_for_record(record),
            data=self.data_for_record(record),
            exc_info=record.exc_info,
        )

    def build_log_record_payload(
        self,
        record: logging.LogRecord,
        diagnostic_logger: JsonlDiagnosticLogger,
    ) -> dict[str, Any]:
        """Build the normalized payload for one LogRecord."""
        return diagnostic_logger.build_payload(
            level=record.levelno,
            component=self.component,
            event=self.event_for_record(record),
            message=self.message_for_record(record),
            logger_name=record.name,
            function=record.funcName,
            invocation_id=self.invocation_id_for_record(record),
            request_id=self.request_id_for_record(record),
            trace_context=self.trace_context_for_record(record),
            correlation_overrides=self.correlation_overrides_for_record(record),
            data=self.data_for_record(record),
            exc_info=record.exc_info,
        )

    def invocation_id_for_record(self, record: logging.LogRecord) -> str | None:
        """Return the invocation ID for one LogRecord, if this component has one."""
        value = getattr(record, "invocation_id", None)
        return value if isinstance(value, str) and value else None

    def request_id_for_record(self, record: logging.LogRecord) -> str | None:
        """Return the request ID for one LogRecord, if this component has one."""
        value = getattr(record, "diagnostic_request_id", None)
        return value if isinstance(value, str) and value else None

    def event_for_record(self, record: logging.LogRecord) -> str:
        """Return the diagnostic event name for one LogRecord."""
        value = getattr(record, "diagnostic_event", None)
        return value if isinstance(value, str) and value else self.event

    def message_for_record(self, record: logging.LogRecord) -> str:
        """Return the diagnostic message for one LogRecord."""
        value = getattr(record, "diagnostic_message", None)
        return value if isinstance(value, str) and value else record.getMessage()

    def trace_context_for_record(
        self,
        record: logging.LogRecord,
    ) -> TraceContext | None:
        """Return explicit record context, falling back to current request/process context."""
        value = getattr(record, "trace_context", None)
        record_context = value if isinstance(value, TraceContext) else None
        return record_context or get_current_trace_context()

    def correlation_overrides_for_record(
        self,
        record: logging.LogRecord,
    ) -> Mapping[str, Any] | None:
        """Return record-level correlation field overrides."""
        return record_correlation_overrides(record)

    def data_for_record(self, record: logging.LogRecord) -> Any:
        """Return the structured payload attached to a LogRecord."""
        return getattr(record, "data", None)

    def should_remove_handler(self, handler: logging.Handler) -> bool:
        """Return whether reset should remove a handler from the Python logger."""
        return handler is self._handler

    def reset_for_tests(self) -> None:
        """Reset attached Python logging handler state for tests."""
        detached: list[logging.Handler] = []
        for python_logger in self.get_python_loggers():
            for handler in list(python_logger.handlers):
                if self.should_remove_handler(handler):
                    python_logger.removeHandler(handler)
                    if handler not in detached:
                        detached.append(handler)
            if self.propagate_on_reset is not None:
                python_logger.propagate = self.propagate_on_reset
        # The same handler is shared by several trees, so it can only be closed
        # once it is detached from all of them.
        for handler in detached:
            handler.close()
        self._handler = None
        super().reset_for_tests()


class DiagnosticLogRecordHandler(logging.Handler):
    """Convert Python LogRecord objects into component diagnostic JSONL records."""

    def __init__(
        self,
        diagnostic_logging: PythonLogRecordDiagnosticLogging,
        diagnostic_logger: JsonlDiagnosticLogger,
    ) -> None:
        super().__init__()
        self._diagnostic_logging = diagnostic_logging
        self._diagnostic_logger = diagnostic_logger

    @property
    def diagnostic_logging(self) -> PythonLogRecordDiagnosticLogging:
        """Return the component mapper used by this handler."""
        return self._diagnostic_logging

    def emit(self, record: logging.LogRecord) -> None:
        """Write one logging record. All handler failures are swallowed."""
        try:
            self._diagnostic_logging.write_log_record(record, self._diagnostic_logger)
        except Exception:  # noqa: BLE001
            pass
