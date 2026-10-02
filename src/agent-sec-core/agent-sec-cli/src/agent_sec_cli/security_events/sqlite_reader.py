"""SQLAlchemy-backed reader for querying security events."""

from pathlib import Path

from agent_sec_cli.security_events.config import get_db_path
from agent_sec_cli.security_events.orm_store import SqliteStore
from agent_sec_cli.security_events.repositories import (
    CorrelationCandidate,
    SecurityEventRepository,
    SecurityEventsSummary,
)
from agent_sec_cli.security_events.schema import SecurityEvent
from sqlalchemy.engine import Engine
from sqlalchemy.orm import Session, sessionmaker


class SqliteEventReader:
    """Read-only SQLAlchemy reader for security events."""

    def __init__(self, path: str | Path | None = None) -> None:
        self._store = SqliteStore(path or get_db_path(), read_only=True)
        self._repository = SecurityEventRepository(self._store)

    @property
    def _engine(self) -> Engine | None:
        return self._store.engine

    @property
    def _session_factory(self) -> sessionmaker[Session] | None:
        return self._store.cached_session_factory

    def _ensure_session_factory(self) -> sessionmaker[Session] | None:
        """Return a lazily initialized read-only session factory."""
        return self._store.session_factory()

    def _dispose_engine(self) -> None:
        """Dispose SQLAlchemy engine state and clear session factory."""
        self._store.dispose()

    def close(self) -> None:
        """Dispose cached read-only connections."""
        self._store.close()

    def query(
        self,
        event_type: str | None = None,
        category: str | None = None,
        result: str | None = None,
        trace_id: str | None = None,
        session_id: str | None = None,
        run_id: str | None = None,
        call_id: str | None = None,
        tool_call_id: str | None = None,
        since: str | None = None,
        until: str | None = None,
        verdict: str | None = None,
        limit: int = 1000,
        offset: int = 0,
    ) -> list[SecurityEvent]:
        """Query security events with optional filters."""
        return self._repository.query(
            event_type=event_type,
            category=category,
            result=result,
            trace_id=trace_id,
            session_id=session_id,
            run_id=run_id,
            call_id=call_id,
            tool_call_id=tool_call_id,
            since=since,
            until=until,
            verdict=verdict,
            limit=limit,
            offset=offset,
        )

    def get(self, event_id: str) -> SecurityEvent | None:
        """Return one security event by id."""
        return self._repository.get(event_id)

    def query_correlation_candidates(
        self,
        *,
        session_id: str,
        categories: tuple[str, ...] | list[str],
        run_id: str | None = None,
        tool_call_id: str | None = None,
        tool_call_ids: tuple[str, ...] | list[str] | None = None,
        since_epoch: float | None = None,
        until_epoch: float | None = None,
    ) -> list[CorrelationCandidate]:
        """Query up to 1000 read-only candidates for observability correlation."""
        return self._repository.query_correlation_candidates(
            session_id=session_id,
            categories=categories,
            run_id=run_id,
            tool_call_id=tool_call_id,
            tool_call_ids=tool_call_ids,
            since_epoch=since_epoch,
            until_epoch=until_epoch,
        )

    def count(
        self,
        event_type: str | None = None,
        category: str | None = None,
        result: str | None = None,
        trace_id: str | None = None,
        session_id: str | None = None,
        run_id: str | None = None,
        call_id: str | None = None,
        tool_call_id: str | None = None,
        since: str | None = None,
        until: str | None = None,
        verdict: str | None = None,
        offset: int = 0,
    ) -> int:
        """Count events matching the given filters."""
        return self._repository.count(
            event_type=event_type,
            category=category,
            result=result,
            trace_id=trace_id,
            session_id=session_id,
            run_id=run_id,
            call_id=call_id,
            tool_call_id=tool_call_id,
            since=since,
            until=until,
            verdict=verdict,
            offset=offset,
        )

    def count_by(
        self,
        group_field: str,
        event_type: str | None = None,
        category: str | None = None,
        result: str | None = None,
        trace_id: str | None = None,
        session_id: str | None = None,
        run_id: str | None = None,
        call_id: str | None = None,
        tool_call_id: str | None = None,
        since: str | None = None,
        until: str | None = None,
        verdict: str | None = None,
        offset: int = 0,
    ) -> dict[str, int]:
        """Count events grouped by a specific field."""
        return self._repository.count_by(
            group_field,
            event_type=event_type,
            category=category,
            result=result,
            trace_id=trace_id,
            session_id=session_id,
            run_id=run_id,
            call_id=call_id,
            tool_call_id=tool_call_id,
            since=since,
            until=until,
            verdict=verdict,
            offset=offset,
        )

    def summary(
        self,
        event_type: str | None = None,
        category: str | None = None,
        result: str | None = None,
        trace_id: str | None = None,
        session_id: str | None = None,
        run_id: str | None = None,
        call_id: str | None = None,
        tool_call_id: str | None = None,
        since: str | None = None,
        until: str | None = None,
        verdict: str | None = None,
        latest_limit: int = 5,
    ) -> SecurityEventsSummary:
        """Return dashboard summary aggregates and latest events."""
        return self._repository.summary(
            event_type=event_type,
            category=category,
            result=result,
            trace_id=trace_id,
            session_id=session_id,
            run_id=run_id,
            call_id=call_id,
            tool_call_id=tool_call_id,
            since=since,
            until=until,
            verdict=verdict,
            latest_limit=latest_limit,
        )
