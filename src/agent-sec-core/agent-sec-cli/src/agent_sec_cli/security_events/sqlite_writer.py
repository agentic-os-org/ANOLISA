"""SQLAlchemy-backed writer for security events.

Runs alongside the existing JSONL writer (dual-write pattern).
All exceptions are swallowed; never raises to callers.
"""

import logging
import threading
from pathlib import Path

from agent_sec_cli.security_events.config import get_db_path
from agent_sec_cli.security_events.models import migrate_security_events_schema
from agent_sec_cli.security_events.orm_store import (
    SqliteStore,
    _is_sqlite_busy_error,
    _is_sqlite_corruption_error,
    _is_sqlite_schema_error,
)
from agent_sec_cli.security_events.repositories import SecurityEventRepository
from agent_sec_cli.security_events.schema import SecurityEvent
from agent_sec_cli.security_events.sqlite_maintenance import (
    run_sqlite_maintenance_if_due,
)
from sqlalchemy.engine import Engine
from sqlalchemy.exc import DatabaseError, SQLAlchemyError
from sqlalchemy.orm import Session, sessionmaker

logger = logging.getLogger(__name__)


class SqliteEventWriter:
    """Fire-and-forget SQLAlchemy writer for security events."""

    def __init__(
        self,
        path: str | Path | None = None,
        max_age_days: int | None = 30,
    ) -> None:
        self._store = SqliteStore(
            path or get_db_path(),
            schema_migrations=migrate_security_events_schema,
        )
        self._repository = SecurityEventRepository(self._store)
        self._max_age_days = max_age_days
        self._write_lock = threading.Lock()

    @property
    def _engine(self) -> Engine | None:
        return self._store.engine

    @property
    def _session_factory(self) -> sessionmaker[Session] | None:
        return self._store.cached_session_factory

    @property
    def _disabled(self) -> bool:
        return self._store.disabled

    def write(self, event: SecurityEvent) -> None:
        """Insert *event* into SQLite. Fire-and-forget; never raises.

        Dropped writes that reach the repository route through
        :meth:`_log_drop` so that a sudden surge of dropped security events is
        observable in ``cli.jsonl`` even when nothing reaches stderr.
        """
        with self._write_lock:
            if self._store.disabled:
                return

            try:
                inserted = self._repository.insert(event)
                if not inserted:
                    self._log_drop(
                        event,
                        RuntimeError("sqlite write was skipped"),
                        "insert",
                    )
                    return
            except DatabaseError as exc:
                if _is_sqlite_busy_error(exc):
                    self._log_drop(event, exc, "insert", busy=True)
                    return
                if not _is_sqlite_corruption_error(exc):
                    if _is_sqlite_schema_error(exc):
                        self._store.request_schema_repair()
                    self._log_drop(event, exc, "insert")
                    return
                self._store.handle_corruption(exc)
                if self._store.disabled:
                    self._log_drop(event, exc, "corruption_disabled")
                    return
                try:
                    self._repository.insert(event)
                except Exception as retry_exc:  # noqa: BLE001
                    busy = _is_sqlite_busy_error(retry_exc)
                    self._log_drop(
                        event,
                        retry_exc,
                        "corruption_retry",
                        busy=busy,
                    )
                    if not busy:
                        self._store.dispose()
            except (SQLAlchemyError, OSError) as exc:
                self._log_drop(event, exc, "io")
                self._store.dispose()
            except Exception as exc:  # noqa: BLE001
                self._log_drop(event, exc, "insert")

    def _log_drop(
        self,
        event: SecurityEvent,
        exc: Exception,
        phase: str,
        *,
        busy: bool = False,
    ) -> None:
        """Emit one best-effort diagnostic record for a dropped SQLite event."""
        original = getattr(exc, "orig", exc)
        message = (
            "sqlite busy dropped security event"
            if busy
            else "sqlite write dropped security event"
        )
        try:
            logger.warning(
                message,
                extra={
                    "trace_id": event.trace_id,
                    "data": {
                        "action": "security_event_sqlite_write",
                        "category": event.category,
                        "error": str(original),
                        "error_type": type(original).__name__,
                        "event_id": event.event_id,
                        "event_type": event.event_type,
                        "phase": phase,
                    },
                },
            )
        except Exception:  # noqa: BLE001
            pass

    def close(self) -> None:
        """Best-effort gated prune/WAL checkpoint and dispose pooled connections."""
        if self._store.engine is None:
            return
        try:
            run_sqlite_maintenance_if_due(self._store.path, self._run_maintenance)
        finally:
            self._store.close()

    def _run_maintenance(self) -> None:
        """Run low-frequency SQLite maintenance for this writer."""
        if self._max_age_days is not None:
            self._repository.prune(self._max_age_days)
        self._repository.checkpoint()
