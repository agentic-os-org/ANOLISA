"""Unit tests for observability SQLite retention."""

import sqlite3
from datetime import datetime, timezone
from pathlib import Path

import pytest
from agent_sec_cli.observability.models import (
    OBSERVABILITY_SQLITE_SCHEMA_VERSION,
    ObservabilityEventRecord,
)
from agent_sec_cli.observability.repositories import (
    ObservabilityEventRepository,
)
from agent_sec_cli.observability.schema import validate_observability_record
from agent_sec_cli.security_events.orm_store import SqliteStore
from sqlalchemy.exc import SQLAlchemyError


def test_retention_prunes_by_observed_at_epoch(tmp_path: Path) -> None:
    store = SqliteStore(
        tmp_path / "observability.db",
        models=(ObservabilityEventRecord,),
        schema_version=OBSERVABILITY_SQLITE_SCHEMA_VERSION,
        log_prefix="[observability]",
    )
    repository = ObservabilityEventRepository(store)
    now = datetime(2026, 5, 11, 12, 0, tzinfo=timezone.utc)

    stale_by_observed_at = validate_observability_record(
        {
            "hook": "before_agent_run",
            "observedAt": "2026-05-01T12:00:00Z",
            "metadata": {"sessionId": "old-session", "runId": "old-run"},
            "metrics": {"prompt": "old"},
        }
    )
    fresh_by_observed_at = validate_observability_record(
        {
            "hook": "before_agent_run",
            "observedAt": "2026-05-10T12:00:00Z",
            "metadata": {"sessionId": "new-session", "runId": "new-run"},
            "metrics": {"prompt": "new"},
        }
    )

    assert repository.insert(stale_by_observed_at)
    assert repository.insert(fresh_by_observed_at)

    repository.prune(7, now=now)

    assert repository.count() == 1
    conn = sqlite3.connect(tmp_path / "observability.db")
    try:
        rows = conn.execute("""
            SELECT session_id
            FROM observability_events
            ORDER BY observed_at_epoch
            """).fetchall()
    finally:
        conn.close()

    assert rows == [("new-session",)]
    store.close()


def test_observability_prune_disposes_store_on_sqlalchemy_error(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    class FailingSessionFactory:
        def begin(self):
            raise SQLAlchemyError("prune failed")

    store = SqliteStore(
        tmp_path / "observability.db",
        models=(ObservabilityEventRecord,),
        schema_version=OBSERVABILITY_SQLITE_SCHEMA_VERSION,
        log_prefix="[observability]",
    )
    repository = ObservabilityEventRepository(store)
    disposed = False

    def fake_dispose() -> None:
        nonlocal disposed
        disposed = True

    monkeypatch.setattr(store, "session_factory", lambda: FailingSessionFactory())
    monkeypatch.setattr(store, "dispose", fake_dispose)

    repository.prune(30)

    assert disposed
