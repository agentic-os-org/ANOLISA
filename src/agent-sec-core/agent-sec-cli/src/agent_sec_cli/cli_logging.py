"""Diagnostic JSONL logging for the agent-sec-cli process."""

import logging
from pathlib import Path

from agent_sec_cli.correlation_context import get_invocation_id
from agent_sec_cli.diagnostic_logging import (
    DiagnosticLoggingConfig,
    DiagnosticLogRecordHandler,
    PythonLogRecordDiagnosticLogging,
)

_LOGGER_NAME = "agent_sec_cli"
# Rust records reach Python through pyo3-log, which names the logger after the
# emitting crate's `log` target — `model_service`, `prompt_scanner.<module>` —
# not after `agent_sec_cli`. Left out of the CLI tree they bubble to a root
# with no handler, hit logging.lastResort on stderr, and are then discarded by
# hooks that capture stderr and parse only stdout.
_RUST_LOGGER_NAMES = ("model_service", "prompt_scanner")
_ENV_LOG_LEVEL = "AGENT_SEC_CLI_LOG_LEVEL"
# Diagnostic stream is sized smaller than the business streams: default
# WARNING volume is low, and DEBUG bursts roll over quickly.
CLI_LOG_MAX_BYTES = 10 * 1024 * 1024
CLI_LOG_BACKUP_COUNT = 5

CliLoggingConfig = DiagnosticLoggingConfig


class CliDiagnosticLogging(PythonLogRecordDiagnosticLogging):
    """CLI-specific mapping from Python logging records to diagnostic JSONL."""

    component = "cli"
    stream = "cli"
    level_env = _ENV_LOG_LEVEL
    default_level = logging.WARNING
    max_bytes = CLI_LOG_MAX_BYTES
    backup_count = CLI_LOG_BACKUP_COUNT
    python_logger_name = _LOGGER_NAME
    extra_logger_names = _RUST_LOGGER_NAMES
    event = "cli_log"
    propagate_on_enable = False
    propagate_on_reset = True

    def create_record_handler(self, path: str | Path) -> "JsonlCliLogHandler":
        """Create the CLI compatibility handler used by setup and tests."""
        return JsonlCliLogHandler(path, diagnostic_logging=self)

    def invocation_id_for_record(self, _record: logging.LogRecord) -> str | None:
        """Return the process-level CLI invocation ID."""
        return get_invocation_id()

    def should_remove_handler(self, handler: logging.Handler) -> bool:
        """Remove every CLI diagnostic handler when tests reset logging."""
        return isinstance(handler, JsonlCliLogHandler)


class JsonlCliLogHandler(DiagnosticLogRecordHandler):
    """Compatibility handler for CLI diagnostic JSONL records."""

    def __init__(
        self,
        path: str | Path,
        *,
        diagnostic_logging: CliDiagnosticLogging | None = None,
    ) -> None:
        self._path = Path(path).expanduser()
        if diagnostic_logging is None:
            diagnostic_logging = _CLI_LOGGING
        diagnostic_logger = diagnostic_logging.create_jsonl_logger(
            path=self._path,
            level=logging.NOTSET,
        )
        super().__init__(diagnostic_logging, diagnostic_logger)


_CLI_LOGGING = CliDiagnosticLogging()


def resolve_cli_logging_config() -> CliLoggingConfig:
    """Resolve CLI diagnostic logging settings from the shared base logic."""
    return _CLI_LOGGING.resolve_config()


def setup_cli_logging() -> None:
    """Idempotently configure diagnostic logging for the agent_sec_cli tree."""
    _CLI_LOGGING.setup()


def _reset_cli_logging_for_tests() -> None:
    """Reset module logging state for in-process unit tests."""
    _CLI_LOGGING.reset_for_tests()
