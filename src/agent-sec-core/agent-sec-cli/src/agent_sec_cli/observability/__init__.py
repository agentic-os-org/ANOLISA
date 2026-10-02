"""Observability payload schema and metric definitions."""

import atexit
from typing import TYPE_CHECKING

from agent_sec_cli.observability.metrics import HOOK_METRIC_ALLOWLIST
from agent_sec_cli.observability.schema import (
    ObservabilityMetadata,
    ObservabilityRecord,
)
from agent_sec_cli.observability.writer import ObservabilityWriter

if TYPE_CHECKING:
    from agent_sec_cli.observability.sqlite_writer import (
        ObservabilitySqliteWriter,
    )

_writer: ObservabilityWriter | None = None
_sqlite_writer: "ObservabilitySqliteWriter | None" = None


def get_writer() -> ObservabilityWriter:
    """Return the module-level JSONL writer (created lazily)."""
    global _writer  # noqa: PLW0603
    if _writer is None:
        _writer = ObservabilityWriter()
    return _writer


def get_sqlite_writer() -> "ObservabilitySqliteWriter":
    """Return the module-level SQLite writer (created lazily)."""
    from agent_sec_cli.observability.sqlite_writer import (  # noqa: PLC0415
        ObservabilitySqliteWriter,
    )

    global _sqlite_writer  # noqa: PLW0603
    if _sqlite_writer is None:
        _sqlite_writer = ObservabilitySqliteWriter()
        atexit.register(_sqlite_writer.close)
    return _sqlite_writer


def record_observability(record: ObservabilityRecord) -> None:
    """Persist *record* to JSONL and the SQLite index."""
    get_writer().write(record)
    get_sqlite_writer().write_or_raise(record)


__all__ = [
    "HOOK_METRIC_ALLOWLIST",
    "ObservabilityMetadata",
    "ObservabilityRecord",
    "get_writer",
    "get_sqlite_writer",
    "record_observability",
]
