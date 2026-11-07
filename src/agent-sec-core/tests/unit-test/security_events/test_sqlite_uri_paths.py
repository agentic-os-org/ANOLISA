"""Read-only SQLite paths must retain literal filesystem characters."""

import sqlite3
from pathlib import Path

import pytest
from sqlalchemy import text
from sqlalchemy.exc import SQLAlchemyError

from agent_sec_cli.observability.schema import validate_observability_record
from agent_sec_cli.observability.sqlite_reader import ObservabilityReader
from agent_sec_cli.observability.sqlite_writer import ObservabilitySqliteWriter
from agent_sec_cli.security_events.orm_store import create_sqlite_engine
from agent_sec_cli.security_events.schema import SecurityEvent
from agent_sec_cli.security_events.sqlite_reader import SqliteEventReader
from agent_sec_cli.security_events.sqlite_writer import SqliteEventWriter


@pytest.mark.parametrize(
    "name",
    ["events#archive.db", "events?archive.db", "events%23.db", "events%3F.db", "events café.db"],
)
@pytest.mark.parametrize("relative", [False, True])
def test_readonly_engine_preserves_literal_path(tmp_path, monkeypatch, name, relative):
    monkeypatch.chdir(tmp_path)
    path = Path(name) if relative else tmp_path / name
    with sqlite3.connect(path) as connection:
        connection.execute("CREATE TABLE records (value TEXT)")
        connection.execute("INSERT INTO records VALUES ('expected')")

    engine = create_sqlite_engine(path, read_only=True)
    try:
        with engine.connect() as connection:
            assert connection.execute(text("SELECT value FROM records")).scalar_one() == "expected"
            assert connection.execute(text("PRAGMA query_only")).scalar_one() == 1
            assert Path(connection.execute(text("PRAGMA database_list")).one()[2]) == path.resolve()
            # The URI must retain mode=ro independently of the query-only pragma.
            connection.execute(text("PRAGMA query_only=OFF"))
            with pytest.raises(SQLAlchemyError, match="readonly"):
                connection.execute(text("INSERT INTO records VALUES ('unexpected')"))
    finally:
        engine.dispose()

    assert sorted(p.name for p in tmp_path.iterdir()) == [name]


@pytest.mark.parametrize("name", ["missing#archive.db", "missing?archive.db", "missing%23.db"])
def test_readonly_engine_does_not_create_a_truncated_path(tmp_path, name):
    engine = create_sqlite_engine(tmp_path / name, read_only=True)
    try:
        with pytest.raises(SQLAlchemyError):
            with engine.connect():
                pass
    finally:
        engine.dispose()

    assert list(tmp_path.iterdir()) == []


def test_readers_round_trip_events_in_literal_data_dir(tmp_path, monkeypatch):
    data_dir = tmp_path / "session #1?100%23"
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(data_dir))
    event = SecurityEvent(event_type="code_scan", category="code_scan", details={})
    security_writer = SqliteEventWriter(max_age_days=None)
    observability_writer = ObservabilitySqliteWriter(max_age_days=None)
    try:
        security_writer.write(event)
        observability_writer.write_or_raise(
            validate_observability_record(
                {
                    "hook": "before_agent_run",
                    "observedAt": "2026-05-16T12:00:00Z",
                    "metadata": {"sessionId": "session-A", "runId": "run-A"},
                    "metrics": {"user_input": "inspect coverage"},
                }
            )
        )
    finally:
        security_writer.close()
        observability_writer.close()

    security_reader = SqliteEventReader()
    observability_reader = ObservabilityReader()
    try:
        assert [record.event_id for record in security_reader.query()] == [event.event_id]
        assert security_reader.count() == 1
        assert [session.session_id for session in observability_reader.list_sessions()] == [
            "session-A"
        ]
        assert observability_reader.count_sessions() == 1
    finally:
        security_reader.close()
        observability_reader.close()

    assert list(tmp_path.iterdir()) == [data_dir]
