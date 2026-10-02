"""Diagnostic JSONL logging for the agent-sec daemon process."""

import logging
from collections.abc import Mapping
from pathlib import Path
from typing import Any

from agent_sec_cli.correlation_context import (
    TraceContext,
    trace_context_to_payload,
)
from agent_sec_cli.daemon.request_context import get_current_daemon_request_id
from agent_sec_cli.diagnostic_logging import (
    DiagnosticLogRecordHandler,
    PythonLogRecordDiagnosticLogging,
)

_LOGGER_NAME = "agent-sec-core.daemon"
_ROOT_LOGGER_NAME = ""
_ENV_LOG_LEVEL = "AGENT_SEC_DAEMON_LOG_LEVEL"
DAEMON_LOG_MAX_BYTES = 10 * 1024 * 1024
DAEMON_LOG_BACKUP_COUNT = 5
_DAEMON_LOGGER = logging.getLogger(_LOGGER_NAME)


class DaemonDiagnosticLogging(PythonLogRecordDiagnosticLogging):
    """Daemon-specific mapping from Python logging records to diagnostic JSONL."""

    component = "daemon"
    stream = "daemon"
    level_env = _ENV_LOG_LEVEL
    default_level = logging.INFO
    max_bytes = DAEMON_LOG_MAX_BYTES
    backup_count = DAEMON_LOG_BACKUP_COUNT
    python_logger_name = _ROOT_LOGGER_NAME
    event = "daemon_log"

    def create_record_handler(self, path: str | Path) -> "JsonlDaemonLogHandler":
        """Create the daemon diagnostic handler used by setup and tests."""
        return JsonlDaemonLogHandler(path, diagnostic_logging=self)

    def should_remove_handler(self, handler: logging.Handler) -> bool:
        """Remove every daemon diagnostic handler when tests reset logging."""
        return isinstance(handler, JsonlDaemonLogHandler)

    def request_id_for_record(
        self,
        record: logging.LogRecord,
    ) -> str | None:
        """Return the daemon request id for one LogRecord, when available."""
        return super().request_id_for_record(record) or get_current_daemon_request_id()


class JsonlDaemonLogHandler(DiagnosticLogRecordHandler):
    """Compatibility handler for daemon diagnostic JSONL records."""

    def __init__(
        self,
        path: str | Path,
        *,
        diagnostic_logging: DaemonDiagnosticLogging | None = None,
    ) -> None:
        self._path = Path(path).expanduser()
        if diagnostic_logging is None:
            diagnostic_logging = _DAEMON_LOGGING
        diagnostic_logger = diagnostic_logging.create_jsonl_logger(
            path=self._path,
            level=logging.NOTSET,
        )
        super().__init__(diagnostic_logging, diagnostic_logger)


_DAEMON_LOGGING = DaemonDiagnosticLogging()


def setup_daemon_logging(path: str | Path | None = None) -> None:
    """Idempotently configure daemon JSONL diagnostic logging."""
    _DAEMON_LOGGING.setup(path=path)


def log_daemon_event(
    *,
    level: int = logging.INFO,
    event: str,
    message: str,
    data: Mapping[str, Any] | None = None,
    request_id: str | None = None,
    trace_context: TraceContext | None = None,
) -> None:
    """Emit one structured daemon event through Python logging."""
    extra: dict[str, Any] = {"diagnostic_event": event}
    if data is not None:
        extra["data"] = data
    if request_id is not None:
        extra["diagnostic_request_id"] = request_id
    if trace_context is not None:
        extra["trace_context"] = trace_context
        extra.update(trace_context_to_payload(trace_context))

    _DAEMON_LOGGER.log(level, message, extra=extra)


def reset_daemon_diagnostic_logging_for_tests() -> None:
    """Reset daemon diagnostic logging state for in-process tests."""
    _DAEMON_LOGGING.reset_for_tests()
