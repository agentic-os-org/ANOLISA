"""SQLAlchemy ORM models for queryable security event storage."""

import json
from typing import Any

from agent_sec_cli.security_events.orm_base import Base
from agent_sec_cli.security_events.orm_store import register_orm_models
from agent_sec_cli.security_events.schema import extract_verdict
from agent_sec_cli.security_events.schema_version import (
    SECURITY_EVENTS_SQLITE_SCHEMA_VERSION,
    SECURITY_EVENTS_VERDICT_SCHEMA_VERSION,
)
from sqlalchemy import Float, Index, Integer, Text, inspect, text
from sqlalchemy.engine import Connection
from sqlalchemy.orm import Mapped, mapped_column

_VERDICT_MIGRATION_BATCH_SIZE = 5000


class SecurityEventRecord(Base):
    """ORM mapping for the queryable security event index."""

    __tablename__ = "security_events"
    __table_args__ = (
        Index("idx_event_type", "event_type"),
        Index("idx_category_epoch", "category", "timestamp_epoch"),
        Index("idx_trace_id", "trace_id"),
        Index("idx_timestamp_epoch", "timestamp_epoch"),
        Index("idx_verdict_timestamp_epoch", "verdict", "timestamp_epoch"),
        Index("idx_session_id_timestamp_epoch", "session_id", "timestamp_epoch"),
        Index("idx_run_id_timestamp_epoch", "run_id", "timestamp_epoch"),
        Index(
            "idx_session_run_timestamp_epoch",
            "session_id",
            "run_id",
            "timestamp_epoch",
        ),
    )
    __schema_columns__: dict[str, str] = {
        "run_id": "TEXT",
        "call_id": "TEXT",
        "tool_call_id": "TEXT",
        "verdict": "TEXT",
    }

    event_id: Mapped[str] = mapped_column(Text, primary_key=True)
    event_type: Mapped[str] = mapped_column(Text, nullable=False)
    category: Mapped[str] = mapped_column(Text, nullable=False)
    result: Mapped[str] = mapped_column(
        Text, nullable=False, server_default="succeeded"
    )
    timestamp: Mapped[str] = mapped_column(Text, nullable=False)
    timestamp_epoch: Mapped[float] = mapped_column(Float, nullable=False)
    trace_id: Mapped[str] = mapped_column(Text, nullable=False, server_default="")
    pid: Mapped[int] = mapped_column(Integer, nullable=False)
    uid: Mapped[int] = mapped_column(Integer, nullable=False)
    session_id: Mapped[str | None] = mapped_column(Text, nullable=True)
    run_id: Mapped[str | None] = mapped_column(Text, nullable=True)
    call_id: Mapped[str | None] = mapped_column(Text, nullable=True)
    tool_call_id: Mapped[str | None] = mapped_column(Text, nullable=True)
    verdict: Mapped[str | None] = mapped_column(Text, nullable=True)
    details: Mapped[str] = mapped_column(Text, nullable=False)


def migrate_security_events_schema(
    conn: Connection,
    from_version: int,
    to_version: int,
    _models: tuple[type[Base], ...],
    _log_prefix: str,
) -> None:
    """Apply security event schema migrations before generic convergence."""
    if from_version < SECURITY_EVENTS_VERDICT_SCHEMA_VERSION <= to_version:
        _migrate_verdict_column(conn)


def _migrate_verdict_column(conn: Connection) -> None:
    inspector = inspect(conn)
    table_name = SecurityEventRecord.__tablename__
    if not inspector.has_table(table_name):
        conn.commit()
        return

    existing = {column["name"] for column in inspector.get_columns(table_name)}
    if "verdict" not in existing:
        conn.execute(text(f"ALTER TABLE {table_name} ADD COLUMN verdict TEXT"))
        conn.commit()

    last_rowid = 0
    while True:
        rows = conn.execute(
            text(
                "SELECT rowid AS row_id, event_id, details "
                "FROM security_events "
                "WHERE verdict IS NULL AND rowid > :last_rowid "
                "ORDER BY rowid "
                "LIMIT :limit"
            ),
            {
                "last_rowid": last_rowid,
                "limit": _VERDICT_MIGRATION_BATCH_SIZE,
            },
        ).all()
        if not rows:
            conn.commit()
            break

        last_rowid = max(int(row._mapping["row_id"]) for row in rows)
        updates = _verdict_updates(rows)
        if updates:
            conn.execute(
                text(
                    "UPDATE security_events "
                    "SET verdict = :verdict "
                    "WHERE event_id = :event_id"
                ),
                updates,
            )
        # Commit each bounded batch so large migrations do not hold a write lock.
        conn.commit()


def _verdict_updates(rows: list[Any]) -> list[dict[str, str]]:
    updates: list[dict[str, str]] = []
    for row in rows:
        mapping = row._mapping
        try:
            details = json.loads(mapping["details"])
        except (json.JSONDecodeError, TypeError):
            continue
        if not isinstance(details, dict):
            continue
        verdict = extract_verdict(details)
        if verdict is not None:
            updates.append({"event_id": str(mapping["event_id"]), "verdict": verdict})
    return updates


ORM_MODELS = (SecurityEventRecord,)
register_orm_models(ORM_MODELS)


__all__ = [
    "ORM_MODELS",
    "SECURITY_EVENTS_SQLITE_SCHEMA_VERSION",
    "SecurityEventRecord",
    "migrate_security_events_schema",
]
