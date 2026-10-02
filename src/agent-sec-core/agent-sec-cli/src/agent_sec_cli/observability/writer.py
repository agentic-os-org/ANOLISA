"""JSONL writer for observability records."""

from pathlib import Path

from agent_sec_cli.observability.config import (
    OBSERVABILITY_LOG_PREFIX,
    OBSERVABILITY_STREAM,
    get_observability_log_path,
)
from agent_sec_cli.observability.schema import ObservabilityRecord
from agent_sec_cli.security_events.writer import JsonlEventWriter

DEFAULT_OBSERVABILITY_MAX_BYTES = 256 * 1024 * 1024
DEFAULT_OBSERVABILITY_BACKUP_COUNT = 3


class ObservabilityWriter:
    """Append observability records to the independent observability JSONL stream."""

    def __init__(
        self,
        path: str | Path | None = None,
        max_bytes: int = DEFAULT_OBSERVABILITY_MAX_BYTES,
        backup_count: int = DEFAULT_OBSERVABILITY_BACKUP_COUNT,
    ) -> None:
        self._writer = JsonlEventWriter(
            path=path or get_observability_log_path(),
            max_bytes=max_bytes,
            backup_count=backup_count,
            error_prefix=OBSERVABILITY_LOG_PREFIX,
        )

    def write(self, record: ObservabilityRecord) -> None:
        """Append one validated observability record."""
        self._writer.write_or_raise(record.to_record())


__all__ = [
    "DEFAULT_OBSERVABILITY_BACKUP_COUNT",
    "DEFAULT_OBSERVABILITY_MAX_BYTES",
    "OBSERVABILITY_STREAM",
    "ObservabilityWriter",
]
