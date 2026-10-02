"""SQLAlchemy-backed read-only reader for observability records.

Mirrors ``security_events/sqlite_reader.py`` — wraps a read-only ``SqliteStore``
and delegates list queries to ``ObservabilityEventRepository``.
"""

from pathlib import Path

from agent_sec_cli.observability.config import (
    OBSERVABILITY_LOG_PREFIX,
    get_observability_db_path,
)
from agent_sec_cli.observability.models import (
    OBSERVABILITY_SQLITE_SCHEMA_VERSION,
    ORM_MODELS,
    ObservabilityEventRecord,
)
from agent_sec_cli.observability.repositories import (
    ObservabilityEventRepository,
    RunSummary,
    SessionSummary,
)
from agent_sec_cli.security_events.orm_store import SqliteStore


class ObservabilityReader:
    """Read-only access to the observability SQLite index."""

    def __init__(self, path: str | Path | None = None) -> None:
        # Pass models / schema_version explicitly. Without them, SqliteStore falls
        # back to the security_events default models (registered at import time),
        # which makes ``warn_readonly_schema_readiness`` print a misleading
        # "missing_tables=['security_events']" warning to stderr against an
        # observability DB. Functionally queries still work, but stderr would be
        # polluted.
        self._store = SqliteStore(
            path or get_observability_db_path(),
            read_only=True,
            models=ORM_MODELS,
            schema_version=OBSERVABILITY_SQLITE_SCHEMA_VERSION,
            log_prefix=OBSERVABILITY_LOG_PREFIX,
        )
        self._repository = ObservabilityEventRepository(self._store)

    def count_sessions(
        self,
        *,
        start_epoch: float | None = None,
        end_epoch: float | None = None,
    ) -> int:
        """Return the number of distinct sessions matching optional time filters."""
        return self._repository.count_sessions(
            start_epoch=start_epoch,
            end_epoch=end_epoch,
        )

    def count_runs(
        self,
        session_id: str,
        *,
        start_epoch: float | None = None,
        end_epoch: float | None = None,
    ) -> int:
        """Return the number of distinct runs in a session matching time filters."""
        return self._repository.count_runs(
            session_id,
            start_epoch=start_epoch,
            end_epoch=end_epoch,
        )

    def list_sessions(
        self,
        *,
        start_epoch: float | None = None,
        end_epoch: float | None = None,
        limit: int | None = None,
        offset: int = 0,
    ) -> list[SessionSummary]:
        """Return all sessions ordered by most recent activity descending."""
        return self._repository.list_sessions(
            start_epoch=start_epoch,
            end_epoch=end_epoch,
            limit=limit,
            offset=offset,
        )

    def list_runs(
        self,
        session_id: str,
        *,
        start_epoch: float | None = None,
        end_epoch: float | None = None,
        limit: int | None = None,
        offset: int = 0,
    ) -> list[RunSummary]:
        """Return all runs in *session_id* ordered chronologically."""
        return self._repository.list_runs(
            session_id,
            start_epoch=start_epoch,
            end_epoch=end_epoch,
            limit=limit,
            offset=offset,
        )

    def list_events(
        self,
        session_id: str,
        run_id: str,
        *,
        start_epoch: float | None = None,
        end_epoch: float | None = None,
        limit: int | None = None,
        offset: int = 0,
    ) -> list[ObservabilityEventRecord]:
        """Return all events for *run_id* in *session_id* ordered ASC."""
        return self._repository.list_events(
            session_id,
            run_id,
            start_epoch=start_epoch,
            end_epoch=end_epoch,
            limit=limit,
            offset=offset,
        )

    def close(self) -> None:
        """Dispose cached read-only connections."""
        self._store.close()


__all__ = ["ObservabilityReader"]
