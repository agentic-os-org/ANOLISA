"""Typed repository for observability SQLite indexing."""

import json
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any

from agent_sec_cli.observability.models import ObservabilityEventRecord
from agent_sec_cli.observability.schema import ObservabilityRecord
from agent_sec_cli.security_events.orm_store import SqliteStore
from sqlalchemy import delete, func, select, text
from sqlalchemy.exc import SQLAlchemyError

_USER_INPUT_PREVIEW_LIMIT = 80


@dataclass(frozen=True)
class SessionSummary:
    """Aggregated stats for one session, used by the review TUI's session list."""

    session_id: str
    first_seen_epoch: float
    last_seen_epoch: float
    turn_count: int
    event_count: int


@dataclass(frozen=True)
class RunSummary:
    """Aggregated stats for one run (one user turn) inside a session."""

    run_id: str
    started_at_epoch: float
    ended_at_epoch: float
    user_input_preview: str | None
    event_count: int


class ObservabilityEventRepository:
    """Repository for observability insert/count/prune operations."""

    def __init__(self, store: SqliteStore) -> None:
        self._store = store

    def insert(self, record: ObservabilityRecord) -> bool:
        """Insert an observability record. Returns False for skipped writes."""
        try:
            return self.insert_or_raise(record)
        except (ValueError, TypeError):
            return False
        except (SQLAlchemyError, OSError):
            self._store.dispose()
            return False

    def insert_or_raise(self, record: ObservabilityRecord) -> bool:
        """Insert an observability record and surface foreground failures.

        Returns ``False`` when the underlying store is disabled and the write
        was skipped without touching SQLite. Raises ``ValueError`` /
        ``TypeError`` when *record* is malformed: this is a caller bug, not an
        I/O fault, so the engine pool must NOT be torn down on its account.
        """
        values = self._record_values(record)

        session_factory = self._store.session_factory(raise_on_error=True)
        if session_factory is None:
            return False

        with session_factory.begin() as session:
            session.add(ObservabilityEventRecord(**values))
        return True

    def count(self) -> int:
        """Return the number of indexed observability records."""
        session_factory = self._store.session_factory()
        if session_factory is None:
            return 0

        try:
            with session_factory() as session:
                return int(
                    session.execute(
                        select(func.count()).select_from(ObservabilityEventRecord)
                    ).scalar_one()
                )
        except SQLAlchemyError:
            self._store.dispose()
            return 0

    def count_sessions(
        self,
        *,
        start_epoch: float | None = None,
        end_epoch: float | None = None,
    ) -> int:
        """Return the number of distinct sessions matching optional time filters."""
        session_factory = self._store.session_factory()
        if session_factory is None:
            return 0

        conditions: list[Any] = []
        if start_epoch is not None:
            conditions.append(ObservabilityEventRecord.observed_at_epoch >= start_epoch)
        if end_epoch is not None:
            conditions.append(ObservabilityEventRecord.observed_at_epoch < end_epoch)

        stmt = select(
            func.count(func.distinct(ObservabilityEventRecord.session_id))
        ).where(*conditions)

        try:
            with session_factory() as session:
                return int(session.execute(stmt).scalar_one())
        except SQLAlchemyError:
            self._store.dispose()
            return 0

    def count_runs(
        self,
        session_id: str,
        *,
        start_epoch: float | None = None,
        end_epoch: float | None = None,
    ) -> int:
        """Return the number of distinct runs in a session matching time filters."""
        session_factory = self._store.session_factory()
        if session_factory is None:
            return 0

        conditions: list[Any] = [ObservabilityEventRecord.session_id == session_id]
        if start_epoch is not None:
            conditions.append(ObservabilityEventRecord.observed_at_epoch >= start_epoch)
        if end_epoch is not None:
            conditions.append(ObservabilityEventRecord.observed_at_epoch < end_epoch)

        stmt = select(func.count(func.distinct(ObservabilityEventRecord.run_id))).where(
            *conditions
        )

        try:
            with session_factory() as session:
                return int(session.execute(stmt).scalar_one())
        except SQLAlchemyError:
            self._store.dispose()
            return 0

    def list_sessions(
        self,
        *,
        start_epoch: float | None = None,
        end_epoch: float | None = None,
        limit: int | None = None,
        offset: int = 0,
    ) -> list[SessionSummary]:
        """Return all sessions ordered by most recent activity descending."""
        session_factory = self._store.session_factory()
        if session_factory is None:
            return []

        conditions: list[Any] = []
        if start_epoch is not None:
            conditions.append(ObservabilityEventRecord.observed_at_epoch >= start_epoch)
        if end_epoch is not None:
            conditions.append(ObservabilityEventRecord.observed_at_epoch < end_epoch)

        stmt = (
            select(
                ObservabilityEventRecord.session_id,
                func.min(ObservabilityEventRecord.observed_at_epoch).label(
                    "first_seen"
                ),
                func.max(ObservabilityEventRecord.observed_at_epoch).label("last_seen"),
                func.count(func.distinct(ObservabilityEventRecord.run_id)).label(
                    "turn_count"
                ),
                func.count().label("event_count"),
            )
            .where(*conditions)
            .group_by(ObservabilityEventRecord.session_id)
            .order_by(func.max(ObservabilityEventRecord.observed_at_epoch).desc())
        )
        if limit is not None:
            stmt = stmt.limit(limit)
        if offset:
            stmt = stmt.offset(offset)

        try:
            with session_factory() as session:
                rows = session.execute(stmt).all()
        except SQLAlchemyError:
            self._store.dispose()
            return []

        return [
            SessionSummary(
                session_id=row.session_id,
                first_seen_epoch=float(row.first_seen),
                last_seen_epoch=float(row.last_seen),
                turn_count=int(row.turn_count),
                event_count=int(row.event_count),
            )
            for row in rows
        ]

    def list_runs(
        self,
        session_id: str,
        *,
        start_epoch: float | None = None,
        end_epoch: float | None = None,
        limit: int | None = None,
        offset: int = 0,
    ) -> list[RunSummary]:
        """Return all runs in *session_id* ordered chronologically.

        Two queries (constant, not N+1):
          1. GROUP BY run_id for stats.
          2. Window query for the first before_agent_run metrics_json per run.
        """
        session_factory = self._store.session_factory()
        if session_factory is None:
            return []

        conditions: list[Any] = [ObservabilityEventRecord.session_id == session_id]
        if start_epoch is not None:
            conditions.append(ObservabilityEventRecord.observed_at_epoch >= start_epoch)
        if end_epoch is not None:
            conditions.append(ObservabilityEventRecord.observed_at_epoch < end_epoch)

        stats_stmt = (
            select(
                ObservabilityEventRecord.run_id,
                func.min(ObservabilityEventRecord.observed_at_epoch).label(
                    "started_at"
                ),
                func.max(ObservabilityEventRecord.observed_at_epoch).label("ended_at"),
                func.count().label("event_count"),
            )
            .where(*conditions)
            .group_by(ObservabilityEventRecord.run_id)
            .order_by(func.min(ObservabilityEventRecord.observed_at_epoch).asc())
        )
        if limit is not None:
            stats_stmt = stats_stmt.limit(limit)
        if offset:
            stats_stmt = stats_stmt.offset(offset)

        first_before_run_subq = (
            select(
                ObservabilityEventRecord.run_id.label("run_id"),
                ObservabilityEventRecord.metrics_json.label("metrics_json"),
                func.row_number()
                .over(
                    partition_by=ObservabilityEventRecord.run_id,
                    order_by=(
                        ObservabilityEventRecord.observed_at_epoch.asc(),
                        ObservabilityEventRecord.id.asc(),
                    ),
                )
                .label("rn"),
            )
            .where(*conditions, ObservabilityEventRecord.hook == "before_agent_run")
            .subquery()
        )
        before_run_stmt = select(
            first_before_run_subq.c.run_id, first_before_run_subq.c.metrics_json
        ).where(first_before_run_subq.c.rn == 1)

        try:
            with session_factory() as session:
                stats_rows = session.execute(stats_stmt).all()
                before_rows = session.execute(before_run_stmt).all()
        except SQLAlchemyError:
            self._store.dispose()
            return []

        first_metrics = {row.run_id: row.metrics_json for row in before_rows}

        return [
            RunSummary(
                run_id=row.run_id,
                started_at_epoch=float(row.started_at),
                ended_at_epoch=float(row.ended_at),
                user_input_preview=_extract_user_input_preview(
                    first_metrics.get(row.run_id)
                ),
                event_count=int(row.event_count),
            )
            for row in stats_rows
        ]

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
        """Return all events for *run_id* in *session_id* ordered ascending."""
        session_factory = self._store.session_factory()
        if session_factory is None:
            return []

        conditions: list[Any] = [
            ObservabilityEventRecord.session_id == session_id,
            ObservabilityEventRecord.run_id == run_id,
        ]
        if start_epoch is not None:
            conditions.append(ObservabilityEventRecord.observed_at_epoch >= start_epoch)
        if end_epoch is not None:
            conditions.append(ObservabilityEventRecord.observed_at_epoch < end_epoch)

        stmt = (
            select(ObservabilityEventRecord)
            .where(*conditions)
            .order_by(ObservabilityEventRecord.observed_at_epoch.asc())
        )
        if limit is not None:
            stmt = stmt.limit(limit)
        if offset:
            stmt = stmt.offset(offset)

        try:
            with session_factory() as session:
                # Detach rows from the session so callers can read attributes after
                # the session closes.
                rows = list(session.execute(stmt).scalars().all())
                for row in rows:
                    session.expunge(row)
        except SQLAlchemyError:
            self._store.dispose()
            return []

        return rows

    def prune(
        self,
        max_age_days: int,
        *,
        now: datetime | None = None,
    ) -> None:
        """Delete rows older than max_age_days by observed_at_epoch."""
        session_factory = self._store.session_factory()
        if session_factory is None:
            return

        cutoff = _epoch(now or datetime.now(timezone.utc)) - (max_age_days * 86400)
        try:
            with session_factory.begin() as session:
                session.execute(
                    delete(ObservabilityEventRecord).where(
                        ObservabilityEventRecord.observed_at_epoch < cutoff
                    )
                )
        except SQLAlchemyError:
            self._store.dispose()

    def checkpoint(self) -> None:
        """Run a best-effort WAL checkpoint on the current engine."""
        engine = self._store.engine
        if engine is None:
            return
        try:
            with engine.connect() as conn:
                conn.execute(text("PRAGMA wal_checkpoint(TRUNCATE)"))
        except Exception:  # noqa: BLE001
            pass

    @staticmethod
    def _record_values(record: ObservabilityRecord) -> dict[str, object]:
        """Build the ORM values dict for INSERT."""
        wire_record = record.to_record()
        metrics = _ensure_mapping(wire_record["metrics"], "metrics")
        metadata = _ensure_mapping(wire_record["metadata"], "metadata")

        return {
            "hook": record.hook,
            "observed_at": str(wire_record["observedAt"]),
            "observed_at_epoch": record.observed_at.timestamp(),
            "session_id": str(metadata["sessionId"]),
            "run_id": str(metadata["runId"]),
            "metrics_json": json.dumps(metrics, ensure_ascii=False),
            "metadata_json": json.dumps(metadata, ensure_ascii=False),
            "call_id": _optional_str(metadata.get("callId")),
            "tool_call_id": _optional_str(metadata.get("toolCallId")),
        }


def _ensure_mapping(value: Any, name: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise TypeError(f"{name} must be an object")
    return value


def _optional_str(value: Any) -> str | None:
    if value is None:
        return None
    return str(value)


def _epoch(value: datetime) -> float:
    if value.tzinfo is None or value.tzinfo.utcoffset(value) is None:
        value = value.replace(tzinfo=timezone.utc)
    return value.timestamp()


def _extract_user_input_preview(metrics_json: str | None) -> str | None:
    """Extract a short preview of user input from a before_agent_run metrics blob.

    Falls back through ``user_input`` → ``prompt`` → ``None``. Truncates to keep the
    list view tidy. Returns ``None`` if the JSON cannot be parsed or both fields are
    missing/empty — UI then renders a placeholder.
    """
    if metrics_json is None:
        return None
    try:
        metrics = json.loads(metrics_json)
    except (ValueError, TypeError):
        return None
    if not isinstance(metrics, dict):
        return None
    candidate = metrics.get("user_input") or metrics.get("prompt")
    if not candidate:
        return None
    return str(candidate)[:_USER_INPUT_PREVIEW_LIMIT]


__all__ = [
    "ObservabilityEventRepository",
    "RunSummary",
    "SessionSummary",
]
