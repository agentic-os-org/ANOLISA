"""Typed repositories backed by the shared SQLite store."""

import json
import logging
import sys
import time
from dataclasses import dataclass
from typing import Any, Sequence

from agent_sec_cli.security_events.models import SecurityEventRecord
from agent_sec_cli.security_events.orm_store import SqliteStore
from agent_sec_cli.security_events.schema import SecurityEvent, extract_verdict
from agent_sec_cli.utils.timestamp import utc_iso_to_epoch
from sqlalchemy import Select, delete, func, literal, select, text, union_all
from sqlalchemy.dialects.sqlite import insert as sqlite_insert
from sqlalchemy.exc import SQLAlchemyError

logger = logging.getLogger(__name__)

_CORRELATION_CANDIDATE_LIMIT = 1000
_SUMMARY_GROUP_FIELDS = ("category", "event_type", "result", "session_id", "run_id")


@dataclass(frozen=True)
class CorrelationCandidate:
    """Security event row plus the original epoch used for correlation sorting."""

    event: SecurityEvent
    timestamp_epoch: float


@dataclass(frozen=True)
class SecurityEventsSummary:
    """Dashboard summary data returned by one repository call."""

    total: int
    by_category: dict[Any, int]
    by_event_type: dict[Any, int]
    by_result: dict[Any, int]
    by_session: dict[Any, int]
    by_run: dict[Any, int]
    latest_events: list[SecurityEvent]


class SecurityEventRepository:
    """Repository for security event insert/query/count/prune operations."""

    _COUNT_BY_COLUMNS = {
        "category": SecurityEventRecord.category,
        "event_type": SecurityEventRecord.event_type,
        "result": SecurityEventRecord.result,
        "trace_id": SecurityEventRecord.trace_id,
        "session_id": SecurityEventRecord.session_id,
        "run_id": SecurityEventRecord.run_id,
        "call_id": SecurityEventRecord.call_id,
        "tool_call_id": SecurityEventRecord.tool_call_id,
        "verdict": SecurityEventRecord.verdict,
    }

    def __init__(self, store: SqliteStore) -> None:
        self._store = store

    def insert(self, event: SecurityEvent) -> bool:
        """Insert an event. Returns False for invalid or skipped writes."""
        try:
            values = self._event_values(event)
        except (ValueError, TypeError):
            return False
        session_factory = self._store.session_factory(raise_on_error=True)
        if session_factory is None:
            return False

        stmt = (
            sqlite_insert(SecurityEventRecord)
            .values(**values)
            .on_conflict_do_nothing(index_elements=[SecurityEventRecord.event_id])
        )
        with session_factory.begin() as session:
            session.execute(stmt)
        return True

    @staticmethod
    def _event_values(event: SecurityEvent) -> dict[str, object]:
        """Build the ORM values dict for INSERT."""
        return {
            "event_id": event.event_id,
            "event_type": event.event_type,
            "category": event.category,
            "result": event.result,
            "timestamp": event.timestamp,
            # SecurityEvent validation normalizes timestamps before repository entry.
            "timestamp_epoch": utc_iso_to_epoch(event.timestamp),
            "trace_id": event.trace_id,
            "pid": event.pid,
            "uid": event.uid,
            "session_id": event.session_id,
            "run_id": event.run_id,
            "call_id": event.call_id,
            "tool_call_id": event.tool_call_id,
            "verdict": extract_verdict(event.details),
            "details": json.dumps(event.details, ensure_ascii=False),
        }

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
        """Query security events with optional filters.

        All filters, including verdict, are applied in SQL.
        """
        conditions = self._build_filters(
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
        )
        stmt = (
            select(SecurityEventRecord)
            .where(*conditions)
            .order_by(SecurityEventRecord.timestamp_epoch.desc())
            .limit(limit)
            .offset(offset)
        )

        session_factory = self._store.session_factory()
        if session_factory is None:
            return []

        try:
            with session_factory() as session:
                records = list(session.scalars(stmt).all())
        except SQLAlchemyError:
            self._store.dispose()
            return []

        events: list[SecurityEvent] = []
        for record in records:
            event = self._record_to_event(record)
            if event is not None:
                events.append(event)

        return events

    def get(self, event_id: str) -> SecurityEvent | None:
        """Return one security event by id."""
        session_factory = self._store.session_factory()
        if session_factory is None:
            return None

        stmt = select(SecurityEventRecord).where(
            SecurityEventRecord.event_id == event_id
        )
        try:
            with session_factory() as session:
                record = session.execute(stmt).scalar_one_or_none()
        except SQLAlchemyError:
            self._store.dispose()
            return None

        if record is None:
            return None
        return self._record_to_event(record)

    def query_correlation_candidates(
        self,
        *,
        session_id: str,
        categories: Sequence[str],
        run_id: str | None = None,
        tool_call_id: str | None = None,
        tool_call_ids: Sequence[str] | None = None,
        since_epoch: float | None = None,
        until_epoch: float | None = None,
    ) -> list[CorrelationCandidate]:
        """Query up to 1000 read-only candidates for observability correlation."""
        if not categories:
            return []

        conditions: list[Any] = [
            SecurityEventRecord.session_id == session_id,
            SecurityEventRecord.category.in_(tuple(categories)),
        ]
        if run_id is not None:
            conditions.append(SecurityEventRecord.run_id == run_id)
        if tool_call_ids is not None:
            normalized_tool_call_ids = tuple(value for value in tool_call_ids if value)
            if not normalized_tool_call_ids:
                return []
            conditions.append(
                SecurityEventRecord.tool_call_id.in_(normalized_tool_call_ids)
            )
        elif tool_call_id is not None:
            conditions.append(SecurityEventRecord.tool_call_id == tool_call_id)
        if since_epoch is not None:
            conditions.append(SecurityEventRecord.timestamp_epoch >= since_epoch)
        if until_epoch is not None:
            conditions.append(SecurityEventRecord.timestamp_epoch <= until_epoch)

        stmt = (
            select(SecurityEventRecord)
            .where(*conditions)
            .order_by(
                SecurityEventRecord.timestamp_epoch.asc(),
                SecurityEventRecord.event_id.asc(),
            )
            .limit(_CORRELATION_CANDIDATE_LIMIT)
        )

        session_factory = self._store.session_factory()
        if session_factory is None:
            return []

        try:
            with session_factory() as session:
                records = list(session.scalars(stmt).all())
        except SQLAlchemyError as exc:
            logger.warning(
                "correlation candidate query failed",
                extra={
                    "session_id": session_id,
                    "run_id": run_id,
                    "data": {"error_type": type(exc).__name__},
                },
            )
            self._store.dispose()
            return []

        candidates: list[CorrelationCandidate] = []
        for record in records:
            event = self._record_to_event(record)
            if event is not None:
                candidates.append(
                    CorrelationCandidate(
                        event=event,
                        timestamp_epoch=record.timestamp_epoch,
                    )
                )
        return candidates

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
        """Count events matching the given filters.

        All filters, including verdict, are applied in SQL.
        """
        conditions = self._build_filters(
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
        )

        if offset:
            source = (
                select(SecurityEventRecord.event_id)
                .where(*conditions)
                .order_by(SecurityEventRecord.timestamp_epoch.desc())
                .offset(offset)
                .subquery()
            )
            stmt: Select[tuple[int]] = select(func.count()).select_from(source)
        else:
            stmt = (
                select(func.count()).select_from(SecurityEventRecord).where(*conditions)
            )

        session_factory = self._store.session_factory()
        if session_factory is None:
            return 0

        try:
            with session_factory() as session:
                return int(session.execute(stmt).scalar_one())
        except SQLAlchemyError:
            self._store.dispose()
            return 0

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
        """Count events grouped by a specific allowlisted field.

        All filters and group fields, including verdict, are handled in SQL.
        """
        if group_field not in self._COUNT_BY_COLUMNS:
            raise ValueError(
                f"Invalid group_field: {group_field!r}. "
                "Must be one of: call_id, category, event_type, result, "
                "run_id, session_id, tool_call_id, trace_id, verdict"
            )

        conditions = self._build_filters(
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
        )

        column = self._COUNT_BY_COLUMNS[group_field]
        if group_field == "verdict" and verdict is None:
            conditions = [
                *conditions,
                SecurityEventRecord.verdict.is_not(None),
                SecurityEventRecord.verdict != "",
            ]
        if offset:
            source = (
                select(column.label("group_value"))
                .where(*conditions)
                .order_by(SecurityEventRecord.timestamp_epoch.desc())
                .offset(offset)
                .subquery()
            )
            stmt = (
                select(source.c.group_value, func.count())
                .select_from(source)
                .group_by(source.c.group_value)
            )
        else:
            stmt = select(column, func.count()).where(*conditions).group_by(column)

        session_factory = self._store.session_factory()
        if session_factory is None:
            return {}

        try:
            with session_factory() as session:
                rows = session.execute(stmt).all()
                return {row[0]: int(row[1]) for row in rows}
        except SQLAlchemyError:
            self._store.dispose()
            return {}

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
        """Return dashboard aggregates and latest rows in one session."""
        conditions = self._build_filters(
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
        )
        group_stmt = self._summary_group_counts_stmt(conditions)
        latest_stmt = (
            select(SecurityEventRecord)
            .where(*conditions)
            .order_by(SecurityEventRecord.timestamp_epoch.desc())
            .limit(latest_limit)
        )

        session_factory = self._store.session_factory()
        if session_factory is None:
            return _empty_summary()

        try:
            with session_factory() as session:
                group_rows = session.execute(group_stmt).all()
                records = list(session.scalars(latest_stmt).all())
        except SQLAlchemyError:
            self._store.dispose()
            return _empty_summary()

        groups = _summary_groups_from_rows(group_rows)
        latest_events: list[SecurityEvent] = []
        for record in records:
            event = self._record_to_event(record)
            if event is not None:
                latest_events.append(event)

        return SecurityEventsSummary(
            total=sum(groups["category"].values()),
            by_category=groups["category"],
            by_event_type=groups["event_type"],
            by_result=groups["result"],
            by_session=groups["session_id"],
            by_run=groups["run_id"],
            latest_events=latest_events,
        )

    def prune(self, max_age_days: int) -> None:
        """Delete rows older than max_age_days."""
        session_factory = self._store.session_factory()
        if session_factory is None:
            return

        cutoff = time.time() - (max_age_days * 86400)

        try:
            with session_factory.begin() as session:
                session.execute(
                    delete(SecurityEventRecord).where(
                        SecurityEventRecord.timestamp_epoch < cutoff
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
    def _timestamp_epoch(value: str) -> float:
        """Convert a repository-bound UTC ISO timestamp to epoch seconds."""
        return utc_iso_to_epoch(value)

    def _build_filters(
        self,
        *,
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
    ) -> list[Any]:
        """Build SQLAlchemy filter expressions from non-None filters."""
        conditions: list[Any] = []
        if event_type is not None:
            conditions.append(SecurityEventRecord.event_type == event_type)
        if category is not None:
            conditions.append(SecurityEventRecord.category == category)
        if result is not None:
            conditions.append(SecurityEventRecord.result == result)
        if trace_id is not None:
            conditions.append(SecurityEventRecord.trace_id == trace_id)
        if session_id is not None:
            conditions.append(SecurityEventRecord.session_id == session_id)
        if run_id is not None:
            conditions.append(SecurityEventRecord.run_id == run_id)
        if call_id is not None:
            conditions.append(SecurityEventRecord.call_id == call_id)
        if tool_call_id is not None:
            conditions.append(SecurityEventRecord.tool_call_id == tool_call_id)
        if verdict is not None:
            conditions.append(SecurityEventRecord.verdict == verdict)
        if since is not None:
            conditions.append(
                SecurityEventRecord.timestamp_epoch >= self._timestamp_epoch(since)
            )
        if until is not None:
            conditions.append(
                SecurityEventRecord.timestamp_epoch < self._timestamp_epoch(until)
            )
        return conditions

    def _summary_group_counts_stmt(self, conditions: list[Any]) -> Any:
        statements = []
        for group_field in _SUMMARY_GROUP_FIELDS:
            column = self._COUNT_BY_COLUMNS[group_field]
            statements.append(
                select(
                    literal(group_field).label("group_field"),
                    column.label("group_value"),
                    func.count().label("count"),
                )
                .where(*conditions)
                .group_by(column)
            )
        return union_all(*statements)

    @staticmethod
    def _record_to_event(record: SecurityEventRecord) -> SecurityEvent | None:
        """Convert an ORM record to SecurityEvent. Returns None on parse error."""
        try:
            return SecurityEvent(
                event_id=record.event_id,
                event_type=record.event_type,
                category=record.category,
                result=record.result,
                timestamp=record.timestamp,
                trace_id=record.trace_id,
                pid=record.pid,
                uid=record.uid,
                session_id=record.session_id,
                run_id=record.run_id,
                call_id=record.call_id,
                tool_call_id=record.tool_call_id,
                details=json.loads(record.details),
            )
        except (json.JSONDecodeError, TypeError, ValueError) as exc:
            print(f"[security_events] malformed row skipped: {exc}", file=sys.stderr)
            return None


def _empty_summary() -> SecurityEventsSummary:
    return SecurityEventsSummary(
        total=0,
        by_category={},
        by_event_type={},
        by_result={},
        by_session={},
        by_run={},
        latest_events=[],
    )


def _summary_groups_from_rows(rows: list[Any]) -> dict[str, dict[Any, int]]:
    groups: dict[str, dict[Any, int]] = {
        group_field: {} for group_field in _SUMMARY_GROUP_FIELDS
    }
    for row in rows:
        group_field = str(row[0])
        if group_field not in groups:
            continue
        groups[group_field][row[1]] = int(row[2])
    return groups
