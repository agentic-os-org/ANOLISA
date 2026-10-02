"""Configuration helpers for observability persistence."""

from agent_sec_cli.security_events.config import (
    get_stream_db_path,
    get_stream_log_path,
)

OBSERVABILITY_STREAM = "observability"
OBSERVABILITY_LOG_PREFIX = "[observability]"
DEFAULT_OBSERVABILITY_RETENTION_DAYS = 7


def get_observability_log_path() -> str:
    """Return the JSONL path for the observability stream."""
    return get_stream_log_path(OBSERVABILITY_STREAM)


def get_observability_db_path() -> str:
    """Return the SQLite path for the observability stream."""
    return get_stream_db_path(OBSERVABILITY_STREAM)


__all__ = [
    "DEFAULT_OBSERVABILITY_RETENTION_DAYS",
    "OBSERVABILITY_LOG_PREFIX",
    "OBSERVABILITY_STREAM",
    "get_observability_db_path",
    "get_observability_log_path",
]
