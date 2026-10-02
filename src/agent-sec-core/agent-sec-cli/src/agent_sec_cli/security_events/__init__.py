"""security_events — fire-and-forget dual-write security event logging (JSONL + SQLite).

Public API
----------
- ``log_event(event)``        — append a ``SecurityEvent`` to both JSONL and SQLite
- ``get_writer()``            — obtain the singleton JSONL ``SecurityEventWriter``
- ``get_sqlite_writer()``     — obtain the singleton ``SqliteEventWriter``
- ``get_reader()``            — obtain the singleton ``SqliteEventReader``
- ``SecurityEvent``           — the canonical event dataclass
"""

import atexit
from typing import TYPE_CHECKING

from agent_sec_cli.security_events.schema import SecurityEvent
from agent_sec_cli.security_events.writer import SecurityEventWriter

if TYPE_CHECKING:
    from agent_sec_cli.security_events.sqlite_reader import SqliteEventReader
    from agent_sec_cli.security_events.sqlite_writer import SqliteEventWriter

_writer: SecurityEventWriter | None = None
_sqlite_writer: "SqliteEventWriter | None" = None
_reader: "SqliteEventReader | None" = None


def get_writer() -> SecurityEventWriter:
    """Return the module-level singleton JSONL writer (created lazily)."""
    global _writer  # noqa: PLW0603
    if _writer is None:
        _writer = SecurityEventWriter()
    return _writer


def get_sqlite_writer() -> "SqliteEventWriter":
    """Return the module-level singleton SQLite writer (created lazily)."""
    from agent_sec_cli.security_events.sqlite_writer import (  # noqa: PLC0415
        SqliteEventWriter,
    )

    global _sqlite_writer  # noqa: PLW0603
    if _sqlite_writer is None:
        _sqlite_writer = SqliteEventWriter()
        atexit.register(_sqlite_writer.close)
    return _sqlite_writer


def get_reader() -> "SqliteEventReader":
    """Return the module-level singleton SQLite reader (created lazily)."""
    from agent_sec_cli.security_events.sqlite_reader import (  # noqa: PLC0415
        SqliteEventReader,
    )

    global _reader  # noqa: PLW0603
    if _reader is None:
        _reader = SqliteEventReader()
    return _reader


def log_event(event: SecurityEvent) -> None:
    """Persist *event* to JSONL and SQLite.

    This is deliberately **fire-and-forget**: any failure in either
    writer is silently swallowed so that callers are never disrupted.
    """
    try:
        get_writer().write(event)
    except Exception:  # noqa: BLE001
        pass
    try:
        get_sqlite_writer().write(event)
    except Exception:  # noqa: BLE001
        pass


__all__ = [
    "log_event",
    "get_writer",
    "get_sqlite_writer",
    "get_reader",
    "SecurityEvent",
]
