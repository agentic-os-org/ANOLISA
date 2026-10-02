"""Unit tests for security_events.sqlite_writer — SqliteEventWriter."""

import json
import logging
import sqlite3
import stat
import threading
import time
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime
from pathlib import Path
from typing import Any
from unittest.mock import patch

import agent_sec_cli.security_events.models as security_event_models
import agent_sec_cli.security_events.sqlite_writer as sqlite_writer_module
import pytest
from agent_sec_cli.security_events.schema import SecurityEvent
from agent_sec_cli.security_events.sqlite_writer import SqliteEventWriter
from sqlalchemy.exc import DatabaseError, SQLAlchemyError

SQLITE_WRITER_LOGGER = "agent_sec_cli.security_events.sqlite_writer"


def _make_event(
    event_type: str = "test_event", category: str = "test", **kwargs: Any
) -> SecurityEvent:
    return SecurityEvent(
        event_type=event_type,
        category=category,
        details=kwargs.get("details", {"key": "value"}),
        trace_id=kwargs.get("trace_id", ""),
    )


def _busy_database_error(message: str = "database is locked") -> DatabaseError:
    class BusyError(Exception):
        sqlite_errorcode = sqlite3.SQLITE_BUSY

    return DatabaseError("INSERT", {}, BusyError(message))


def _locked_database_error(message: str = "database table is locked") -> DatabaseError:
    class LockedError(Exception):
        sqlite_errorcode = sqlite3.SQLITE_LOCKED

    return DatabaseError("INSERT", {}, LockedError(message))


@pytest.fixture()
def db_path(tmp_path: Path) -> str:
    return str(tmp_path / "test.db")


class TestSqliteEventWriter:
    def test_write_with_invalid_timestamp(
        self,
        db_path: str,
        caplog: pytest.LogCaptureFixture,
        capsys: pytest.CaptureFixture[str],
    ) -> None:
        """Invalid timestamps are dropped without writing to stderr."""
        writer = SqliteEventWriter(path=db_path)

        # Bypass schema validation to exercise writer drop behavior for
        # corrupted/in-memory events.
        evt = SecurityEvent.model_construct(
            event_id="invalid-timestamp-event",
            event_type="test",
            category="test",
            result="succeeded",
            trace_id="",
            details={"key": "value"},
            timestamp="not-a-valid-timestamp",
            pid=1,
            uid=1,
            session_id=None,
            run_id=None,
            call_id=None,
            tool_call_id=None,
        )

        with caplog.at_level(logging.WARNING, logger=SQLITE_WRITER_LOGGER):
            writer.write(evt)

        assert capsys.readouterr().err == ""
        matching = [
            record
            for record in caplog.records
            if record.name == SQLITE_WRITER_LOGGER
            and record.message == "sqlite write dropped security event"
        ]
        assert len(matching) == 1
        assert matching[0].data["phase"] == "insert"
        assert matching[0].data["event_id"] == evt.event_id

        # DB file should not be created since write fails before connection
        assert not Path(db_path).exists()

        writer.close()

    def test_write_with_non_serializable_details(
        self,
        db_path: str,
        caplog: pytest.LogCaptureFixture,
        capsys: pytest.CaptureFixture[str],
    ) -> None:
        """Non-serializable details are dropped without writing to stderr."""
        writer = SqliteEventWriter(path=db_path)

        # Create event with non-serializable details (custom object)
        class CustomObject:
            pass

        evt = SecurityEvent(
            event_type="test",
            category="test",
            details={"obj": CustomObject()},  # json.dumps will fail
        )

        with caplog.at_level(logging.WARNING, logger=SQLITE_WRITER_LOGGER):
            writer.write(evt)

        assert capsys.readouterr().err == ""
        matching = [
            record
            for record in caplog.records
            if record.name == SQLITE_WRITER_LOGGER
            and record.message == "sqlite write dropped security event"
        ]
        assert len(matching) == 1
        assert matching[0].data["phase"] == "insert"
        assert matching[0].data["event_id"] == evt.event_id

        # DB file should not be created since write fails before connection
        assert not Path(db_path).exists()

        writer.close()

    def test_write_column_values_are_correct(self, db_path: str) -> None:
        """Verify that all column values are correctly written to SQLite.

        This is a critical data integrity test — validates the entire
        SecurityEvent conversion and INSERT correctness.
        """
        writer = SqliteEventWriter(path=db_path, max_age_days=None)

        # Create a comprehensive event with all fields
        evt = SecurityEvent(
            event_type="harden",
            category="hardening",
            result="failed",
            timestamp="2026-04-20T13:47:00.123456+00:00",
            trace_id="test-trace-123",
            session_id="session-abc",
            run_id="run-abc",
            call_id="call-abc",
            tool_call_id="tool-abc",
            details={
                "result": {"verdict": "deny"},
                "nested": {"key": "value"},
                "list": [1, 2, 3],
                "unicode": "测试中文",
                "null_value": None,
                "empty_string": "",
            },
        )

        writer.write(evt)
        writer.close()

        # Directly query the database to verify all column values
        conn = sqlite3.connect(db_path)
        conn.row_factory = sqlite3.Row
        row = conn.execute("SELECT * FROM security_events").fetchone()
        conn.close()

        # Verify all columns
        assert row is not None
        assert row["event_id"] == evt.event_id
        assert row["event_type"] == "harden"
        assert row["category"] == "hardening"
        assert row["result"] == "failed"
        assert row["timestamp"] == "2026-04-20T13:47:00.123456+00:00"
        assert row["trace_id"] == "test-trace-123"
        assert row["pid"] == evt.pid
        assert row["uid"] == evt.uid
        assert row["session_id"] == "session-abc"
        assert row["run_id"] == "run-abc"
        assert row["call_id"] == "call-abc"
        assert row["tool_call_id"] == "tool-abc"
        assert row["verdict"] == "deny"

        # Verify timestamp_epoch is correct
        expected_epoch = datetime.fromisoformat(evt.timestamp).timestamp()
        assert abs(row["timestamp_epoch"] - expected_epoch) < 0.001

        # Verify details JSON serialization
        details_dict = json.loads(row["details"])
        assert details_dict["nested"] == {"key": "value"}
        assert details_dict["list"] == [1, 2, 3]
        assert details_dict["unicode"] == "测试中文"
        assert details_dict["null_value"] is None
        assert details_dict["empty_string"] == ""

    def test_write_creates_db_and_inserts(self, db_path: str) -> None:
        writer = SqliteEventWriter(path=db_path)
        evt = _make_event()
        writer.write(evt)

        assert Path(db_path).exists()

        conn = sqlite3.connect(db_path)
        rows = conn.execute("SELECT * FROM security_events").fetchall()
        conn.close()
        assert len(rows) == 1
        writer.close()

    def test_write_persists_schema_normalized_utc_timestamp(self, db_path: str) -> None:
        writer = SqliteEventWriter(path=db_path, max_age_days=None)
        writer.write(
            SecurityEvent(
                event_type="offset_timestamp",
                category="test",
                timestamp="2026-05-20T12:00:00+08:00",
                details={},
            )
        )
        writer.close()

        conn = sqlite3.connect(db_path)
        row = conn.execute(
            "SELECT timestamp, timestamp_epoch FROM security_events"
        ).fetchone()
        conn.close()

        assert row == ("2026-05-20T04:00:00+00:00", 1779249600.0)

    def test_db_file_permissions_are_restrictive(self, db_path: str) -> None:
        """Verify that the database file is created with 0o600 permissions."""
        writer = SqliteEventWriter(path=db_path)
        writer.write(_make_event())

        # Check file permissions
        file_stat = Path(db_path).stat()
        mode = stat.S_IMODE(file_stat.st_mode)

        # Should be 0o600 (owner read/write only)
        assert (
            mode == 0o600
        ), f"Database file has permissions {oct(mode)}, expected 0o600"

        writer.close()

    def test_wal_mode_enabled(self, db_path: str) -> None:
        writer = SqliteEventWriter(path=db_path)
        writer.write(_make_event())

        conn = sqlite3.connect(db_path)
        mode = conn.execute("PRAGMA journal_mode").fetchone()[0]
        conn.close()
        assert mode == "wal"
        writer.close()

    def test_fire_and_forget_never_raises(self) -> None:
        invalid_path = "/nonexistent/dir/test.db"
        writer = SqliteEventWriter(path=invalid_path)
        # Should not raise
        writer.write(_make_event())

    def test_write_swallows_unexpected_insert_exception(
        self,
        db_path: str,
        caplog: pytest.LogCaptureFixture,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        event = _make_event(event_type="unexpected_insert_error")
        writer = SqliteEventWriter(path=db_path)

        def raise_runtime_error(_event: SecurityEvent) -> bool:
            raise RuntimeError("unexpected insert failure")

        monkeypatch.setattr(writer._repository, "insert", raise_runtime_error)

        with caplog.at_level(logging.WARNING, logger=SQLITE_WRITER_LOGGER):
            writer.write(event)

        matching = [
            record
            for record in caplog.records
            if record.name == SQLITE_WRITER_LOGGER
            and record.message == "sqlite write dropped security event"
        ]
        assert len(matching) == 1
        assert matching[0].data["phase"] == "insert"
        assert matching[0].data["event_id"] == event.event_id
        assert matching[0].data["error_type"] == "RuntimeError"

    def test_insert_or_ignore_dedup(self, db_path: str) -> None:
        writer = SqliteEventWriter(path=db_path)
        evt = _make_event()
        writer.write(evt)
        writer.write(evt)  # Same event_id

        conn = sqlite3.connect(db_path)
        count = conn.execute("SELECT COUNT(*) FROM security_events").fetchone()[0]
        conn.close()
        assert count == 1
        writer.close()

    def test_thread_safety(self, db_path: str) -> None:
        writer = SqliteEventWriter(path=db_path)
        errors: list[Exception] = []

        def write_events(thread_id: int) -> None:
            try:
                for i in range(10):
                    writer.write(
                        _make_event(
                            trace_id=f"thread-{thread_id}-event-{i}",
                        )
                    )
            except Exception as e:
                errors.append(e)

        threads = [threading.Thread(target=write_events, args=(t,)) for t in range(10)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        assert len(errors) == 0

        conn = sqlite3.connect(db_path)
        count = conn.execute("SELECT COUNT(*) FROM security_events").fetchone()[0]
        conn.close()
        assert count == 100
        writer.close()

    def test_concurrent_writes_from_independent_writers(
        self, db_path: str, caplog: pytest.LogCaptureFixture
    ) -> None:
        writer_count = 16
        events_per_writer = 50
        warmup_writer = SqliteEventWriter(path=db_path)
        warmup_writer.write(
            SecurityEvent(
                event_id="warmup-event",
                event_type="warmup_event",
                category="test",
                details={},
            )
        )
        warmup_writer.close()
        writers = [SqliteEventWriter(path=db_path) for _ in range(writer_count)]

        def write_events(writer_id: int) -> None:
            writer = writers[writer_id]
            for event_num in range(events_per_writer):
                writer.write(
                    SecurityEvent(
                        event_id=f"writer-{writer_id}-event-{event_num}",
                        event_type="concurrent_event",
                        category="test",
                        trace_id=f"writer-{writer_id}",
                        details={"writer_id": writer_id, "event_num": event_num},
                    )
                )

        with caplog.at_level(logging.WARNING, logger=SQLITE_WRITER_LOGGER):
            try:
                with ThreadPoolExecutor(max_workers=writer_count) as executor:
                    futures = [
                        executor.submit(write_events, writer_id)
                        for writer_id in range(writer_count)
                    ]
                    for future in as_completed(futures):
                        future.result()
            finally:
                for writer in writers:
                    writer.close()

        conn = sqlite3.connect(db_path)
        total, distinct_ids = conn.execute(
            "SELECT COUNT(*), COUNT(DISTINCT event_id) "
            "FROM security_events WHERE event_type = 'concurrent_event'"
        ).fetchone()
        conn.close()

        expected = writer_count * events_per_writer
        busy_wait_records = [
            record
            for record in caplog.records
            if record.name == SQLITE_WRITER_LOGGER
            and record.message == "sqlite busy dropped security event"
        ]
        assert total + len(busy_wait_records) == expected
        assert distinct_ids == total

    def test_concurrent_cold_bootstrap_is_best_effort(
        self, db_path: str, capsys: pytest.CaptureFixture[str]
    ) -> None:
        writer_count = 4
        events_per_writer = 5

        def write_events(writer_id: int) -> None:
            writer = SqliteEventWriter(path=db_path)
            try:
                for event_num in range(events_per_writer):
                    writer.write(
                        SecurityEvent(
                            event_id=f"cold-{writer_id}-event-{event_num}",
                            event_type="cold_bootstrap_event",
                            category="test",
                            trace_id=f"cold-{writer_id}",
                            details={"writer_id": writer_id, "event_num": event_num},
                        )
                    )
            finally:
                writer.close()

        with ThreadPoolExecutor(max_workers=writer_count) as executor:
            futures = [
                executor.submit(write_events, writer_id)
                for writer_id in range(writer_count)
            ]
            for future in as_completed(futures):
                future.result()

        probe = SqliteEventWriter(path=db_path)
        probe.write(
            SecurityEvent(
                event_id="cold-bootstrap-probe",
                event_type="cold_bootstrap_probe",
                category="test",
                details={},
            )
        )
        probe.close()

        conn = sqlite3.connect(db_path)
        total, distinct_ids = conn.execute(
            "SELECT COUNT(*), COUNT(DISTINCT event_id) "
            "FROM security_events "
            "WHERE event_type IN ('cold_bootstrap_event', 'cold_bootstrap_probe')"
        ).fetchone()
        conn.close()

        assert 1 <= total <= (writer_count * events_per_writer) + 1
        assert distinct_ids == total
        assert "corrupt DB detected" not in capsys.readouterr().err

    def test_pruning_at_close(self, db_path: str) -> None:
        """Pruning happens in close(), not during writes.

        agent-sec-cli is short-lived: each invocation is a separate process,
        so counter-based pruning inside write() would never accumulate across
        invocations.  Instead, close() (called via atexit) prunes once per
        process lifetime.
        """
        writer = SqliteEventWriter(path=db_path, max_age_days=0)

        for _ in range(10):
            writer.write(_make_event())
        time.sleep(0.01)  # Ensure events are in the past relative to close()

        # Before close: all events still present
        conn = sqlite3.connect(db_path)
        count_before = conn.execute("SELECT COUNT(*) FROM security_events").fetchone()[
            0
        ]
        conn.close()
        assert count_before == 10

        # After close: pruning removes events (max_age_days=0 means cutoff=now)
        writer.close()

        conn = sqlite3.connect(db_path)
        count_after = conn.execute("SELECT COUNT(*) FROM security_events").fetchone()[0]
        conn.close()
        assert count_after < 10

    def test_corruption_detection_and_rebuild(self, db_path: str) -> None:
        writer = SqliteEventWriter(path=db_path)
        writer.write(_make_event())
        writer.close()

        # Corrupt the DB file
        with open(db_path, "r+b") as f:
            f.write(b"CORRUPT_GARBAGE" * 100)

        # Create new writer on same path — should detect corruption, delete,
        # recreate fresh DB, and successfully write the current event
        writer2 = SqliteEventWriter(path=db_path)
        writer2.write(_make_event())

        # Fresh DB should exist with the event (no event dropped)
        conn = sqlite3.connect(db_path)
        count = conn.execute("SELECT COUNT(*) FROM security_events").fetchone()[0]
        conn.close()
        assert count == 1
        writer2.close()

    def test_schema_migration_adds_columns(self, db_path: str) -> None:
        # _COLUMNS dict is currently empty, so just verify _ensure_schema runs without error
        writer = SqliteEventWriter(path=db_path)
        writer.write(_make_event())

        conn = sqlite3.connect(db_path)
        columns = {row[1] for row in conn.execute("PRAGMA table_info(security_events)")}
        conn.close()
        # Verify core columns exist
        assert "event_id" in columns
        assert "event_type" in columns
        assert "category" in columns
        assert "timestamp_epoch" in columns
        writer.close()

    def test_security_events_has_tracing_columns_and_indexes(
        self, db_path: str
    ) -> None:
        writer = SqliteEventWriter(path=db_path)
        writer.write(
            SecurityEvent(
                event_type="code_scan",
                category="code_scan",
                details={},
                trace_id="trace-1",
                session_id="session-1",
                run_id="run-1",
                call_id="call-1",
                tool_call_id="tool-1",
            )
        )
        writer.close()

        conn = sqlite3.connect(db_path)
        columns = {row[1] for row in conn.execute("PRAGMA table_info(security_events)")}
        indexes = {row[1] for row in conn.execute("PRAGMA index_list(security_events)")}
        user_version = conn.execute("PRAGMA user_version").fetchone()[0]
        conn.close()

        assert user_version == 3
        assert {
            "session_id",
            "run_id",
            "call_id",
            "tool_call_id",
            "verdict",
        }.issubset(columns)
        assert "idx_verdict_timestamp_epoch" in indexes
        assert "idx_session_id_timestamp_epoch" in indexes
        assert "idx_run_id_timestamp_epoch" in indexes
        assert "idx_session_run_timestamp_epoch" in indexes
        assert "idx_call_id_not_null" not in indexes
        assert "idx_tool_call_id_not_null" not in indexes

    def test_v1_database_migrates_on_write_and_preserves_old_rows(
        self, db_path: str
    ) -> None:
        conn = sqlite3.connect(db_path)
        conn.executescript("""
            CREATE TABLE security_events (
                event_id TEXT PRIMARY KEY,
                event_type TEXT NOT NULL,
                category TEXT NOT NULL,
                result TEXT NOT NULL DEFAULT 'succeeded',
                timestamp TEXT NOT NULL,
                timestamp_epoch FLOAT NOT NULL,
                trace_id TEXT NOT NULL DEFAULT '',
                pid INTEGER NOT NULL,
                uid INTEGER NOT NULL,
                session_id TEXT,
                details TEXT NOT NULL
            );
            PRAGMA user_version = 1;
            """)
        conn.execute("""
            INSERT INTO security_events (
                event_id, event_type, category, result, timestamp, timestamp_epoch,
                trace_id, pid, uid, session_id, details
            ) VALUES (
                'old-event', 'code_scan', 'code_scan', 'succeeded',
                '2026-05-19T00:00:00+00:00', 1779148800.0,
                'old-trace', 1, 1, 'old-session',
                '{"result": {"verdict": "warn"}}'
            )
            """)
        conn.commit()
        conn.close()

        writer = SqliteEventWriter(path=db_path, max_age_days=None)
        writer.write(
            SecurityEvent(
                event_type="prompt_scan",
                category="prompt_scan",
                details={},
                trace_id="new-trace",
                session_id="new-session",
                run_id="new-run",
                call_id="new-call",
                tool_call_id="new-tool",
            )
        )
        writer.close()

        conn = sqlite3.connect(db_path)
        rows = conn.execute(
            "SELECT event_id, run_id, verdict FROM security_events ORDER BY event_id"
        ).fetchall()
        user_version = conn.execute("PRAGMA user_version").fetchone()[0]
        conn.close()

        assert user_version == 3
        assert ("old-event", None, "warn") in rows
        assert any(
            event_id != "old-event" and run_id == "new-run"
            for event_id, run_id, _verdict in rows
        )

    def test_v2_database_migrates_verdict_column_and_backfills(
        self, db_path: str
    ) -> None:
        conn = sqlite3.connect(db_path)
        conn.executescript("""
            CREATE TABLE security_events (
                event_id TEXT PRIMARY KEY,
                event_type TEXT NOT NULL,
                category TEXT NOT NULL,
                result TEXT NOT NULL DEFAULT 'succeeded',
                timestamp TEXT NOT NULL,
                timestamp_epoch FLOAT NOT NULL,
                trace_id TEXT NOT NULL DEFAULT '',
                pid INTEGER NOT NULL,
                uid INTEGER NOT NULL,
                session_id TEXT,
                run_id TEXT,
                call_id TEXT,
                tool_call_id TEXT,
                details TEXT NOT NULL
            );
            PRAGMA user_version = 2;
            """)
        conn.executemany(
            """
            INSERT INTO security_events (
                event_id, event_type, category, result, timestamp, timestamp_epoch,
                trace_id, pid, uid, session_id, run_id, call_id, tool_call_id, details
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            [
                (
                    "direct-verdict",
                    "prompt_scan",
                    "prompt_scan",
                    "succeeded",
                    "2026-05-19T00:00:00+00:00",
                    1779148800.0,
                    "trace-1",
                    1,
                    1,
                    "session-1",
                    "run-1",
                    None,
                    None,
                    '{"verdict": "deny"}',
                ),
                (
                    "nested-verdict",
                    "code_scan",
                    "code_scan",
                    "succeeded",
                    "2026-05-19T00:00:01+00:00",
                    1779148801.0,
                    "trace-2",
                    1,
                    1,
                    "session-1",
                    "run-1",
                    None,
                    None,
                    '{"result": {"verdict": "pass"}}',
                ),
                (
                    "no-verdict",
                    "harden",
                    "hardening",
                    "succeeded",
                    "2026-05-19T00:00:02+00:00",
                    1779148802.0,
                    "trace-3",
                    1,
                    1,
                    "session-1",
                    "run-1",
                    None,
                    None,
                    "{}",
                ),
            ],
        )
        conn.commit()
        conn.close()

        writer = SqliteEventWriter(path=db_path, max_age_days=None)
        writer.write(
            SecurityEvent(
                event_id="new-event",
                event_type="pii_scan",
                category="pii_scan",
                details={"verdict": "warn"},
            )
        )
        writer.close()

        conn = sqlite3.connect(db_path)
        columns = {row[1] for row in conn.execute("PRAGMA table_info(security_events)")}
        indexes = {row[1] for row in conn.execute("PRAGMA index_list(security_events)")}
        rows = dict(
            conn.execute(
                "SELECT event_id, verdict FROM security_events ORDER BY event_id"
            ).fetchall()
        )
        user_version = conn.execute("PRAGMA user_version").fetchone()[0]
        conn.close()

        assert user_version == 3
        assert "verdict" in columns
        assert "idx_verdict_timestamp_epoch" in indexes
        assert rows == {
            "direct-verdict": "deny",
            "nested-verdict": "pass",
            "new-event": "warn",
            "no-verdict": None,
        }

    def test_v2_database_migrates_verdict_column_in_batches(
        self,
        db_path: str,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        monkeypatch.setattr(
            security_event_models,
            "_VERDICT_MIGRATION_BATCH_SIZE",
            2,
        )
        conn = sqlite3.connect(db_path)
        conn.executescript("""
            CREATE TABLE security_events (
                event_id TEXT PRIMARY KEY,
                event_type TEXT NOT NULL,
                category TEXT NOT NULL,
                result TEXT NOT NULL DEFAULT 'succeeded',
                timestamp TEXT NOT NULL,
                timestamp_epoch FLOAT NOT NULL,
                trace_id TEXT NOT NULL DEFAULT '',
                pid INTEGER NOT NULL,
                uid INTEGER NOT NULL,
                session_id TEXT,
                run_id TEXT,
                call_id TEXT,
                tool_call_id TEXT,
                details TEXT NOT NULL
            );
            PRAGMA user_version = 2;
            """)
        conn.executemany(
            """
            INSERT INTO security_events (
                event_id, event_type, category, result, timestamp, timestamp_epoch,
                trace_id, pid, uid, session_id, run_id, call_id, tool_call_id, details
            ) VALUES (?, 'scan', 'prompt_scan', 'succeeded',
                '2026-05-19T00:00:00+00:00', ?, '', 1, 1, NULL, NULL, NULL, NULL, ?)
            """,
            [
                ("row-1", 1779148800.0, '{"verdict": "deny"}'),
                ("row-2-no-verdict", 1779148801.0, "{}"),
                ("row-3", 1779148802.0, '{"verdict": "pass"}'),
                ("row-4", 1779148803.0, '{"result": {"verdict": "warn"}}'),
                ("row-5", 1779148804.0, '{"verdict": "deny"}'),
            ],
        )
        conn.commit()
        conn.close()

        writer = SqliteEventWriter(path=db_path, max_age_days=None)
        writer.write(_make_event(event_id="new-event"))
        writer.close()

        conn = sqlite3.connect(db_path)
        rows = dict(
            conn.execute(
                "SELECT event_id, verdict FROM security_events "
                "WHERE event_id LIKE 'row-%' ORDER BY event_id"
            ).fetchall()
        )
        conn.close()

        assert rows == {
            "row-1": "deny",
            "row-2-no-verdict": None,
            "row-3": "pass",
            "row-4": "warn",
            "row-5": "deny",
        }

    def test_schema_repairs_missing_indexes(self, db_path: str) -> None:
        conn = sqlite3.connect(db_path)
        conn.execute(
            "CREATE TABLE security_events ("
            "event_id TEXT PRIMARY KEY, "
            "event_type TEXT NOT NULL, "
            "category TEXT NOT NULL, "
            "result TEXT NOT NULL DEFAULT 'succeeded', "
            "timestamp TEXT NOT NULL, "
            "timestamp_epoch FLOAT NOT NULL, "
            "trace_id TEXT NOT NULL DEFAULT '', "
            "pid INTEGER NOT NULL, "
            "uid INTEGER NOT NULL, "
            "session_id TEXT, "
            "details TEXT NOT NULL"
            ")"
        )
        conn.commit()
        conn.close()

        writer = SqliteEventWriter(path=db_path)
        writer.write(_make_event())
        writer.close()

        conn = sqlite3.connect(db_path)
        index_names = {
            row[1] for row in conn.execute("PRAGMA index_list(security_events)")
        }
        conn.close()

        assert {
            "idx_event_type",
            "idx_category_epoch",
            "idx_trace_id",
            "idx_timestamp_epoch",
            "idx_verdict_timestamp_epoch",
        }.issubset(index_names)

    def test_schema_error_requests_repair_for_next_write(self, db_path: str) -> None:
        conn = sqlite3.connect(db_path)
        conn.execute("PRAGMA user_version = 3")
        conn.commit()
        conn.close()

        writer = SqliteEventWriter(path=db_path)
        skipped_event = SecurityEvent(
            event_id="schema-error-skipped",
            event_type="schema_repair",
            category="test",
            details={},
        )
        repaired_event = SecurityEvent(
            event_id="schema-error-repaired",
            event_type="schema_repair",
            category="test",
            details={},
        )

        writer.write(skipped_event)
        writer.write(repaired_event)
        writer.close()

        conn = sqlite3.connect(db_path)
        rows = conn.execute(
            "SELECT event_id FROM security_events "
            "WHERE event_type = 'schema_repair' ORDER BY event_id"
        ).fetchall()
        conn.close()

        assert rows == [("schema-error-repaired",)]

    def test_close_performs_checkpoint(self, db_path: str) -> None:
        writer = SqliteEventWriter(path=db_path)
        writer.write(_make_event())
        writer.write(_make_event())

        writer.close()
        assert writer._engine is None
        assert writer._session_factory is None

    def test_close_runs_prune_and_checkpoint_through_maintenance_gate(
        self,
        db_path: str,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        writer = SqliteEventWriter(path=db_path)
        writer.write(_make_event())
        gated_paths: list[Path] = []

        def fake_run_sqlite_maintenance_if_due(
            db_path_arg: str | Path,
            maintenance: Callable[[], None],
            *,
            interval_seconds: float = 0,
            now: float | None = None,
        ) -> bool:
            gated_paths.append(Path(db_path_arg))
            maintenance()
            return True

        monkeypatch.setattr(
            sqlite_writer_module,
            "run_sqlite_maintenance_if_due",
            fake_run_sqlite_maintenance_if_due,
            raising=False,
        )

        writer.close()

        assert gated_paths == [Path(db_path).resolve()]
        assert writer._engine is None
        assert writer._session_factory is None

    def test_close_skips_repeated_maintenance_for_same_db_path(
        self,
        db_path: str,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        first_writer = SqliteEventWriter(path=db_path)
        first_writer.write(_make_event())
        first_writer.close()

        second_writer = SqliteEventWriter(path=db_path)
        second_writer.write(_make_event(event_type="second_event"))

        def fail_if_maintenance_runs() -> None:
            raise AssertionError("maintenance should be skipped while marker is fresh")

        monkeypatch.setattr(
            second_writer,
            "_run_maintenance",
            fail_if_maintenance_runs,
        )

        second_writer.close()

    def test_disabled_after_delete_failure(self, db_path: str) -> None:
        writer = SqliteEventWriter(path=db_path)
        writer.write(_make_event())
        writer.close()

        # Corrupt the DB
        with open(db_path, "r+b") as f:
            f.write(b"CORRUPT_GARBAGE" * 100)

        writer2 = SqliteEventWriter(path=db_path)

        # Mock Path.unlink to raise OSError
        with patch.object(Path, "unlink", side_effect=OSError("permission denied")):
            writer2.write(_make_event())

        # Writer should be disabled now
        assert writer2._disabled

        # Subsequent writes should be no-ops
        writer2.write(_make_event())

    def test_write_retries_after_corruption_error(
        self, db_path: str, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        class CorruptError(Exception):
            sqlite_errorcode = sqlite3.SQLITE_CORRUPT

        writer = SqliteEventWriter(path=db_path)
        calls = 0

        def flaky_insert(_event: SecurityEvent) -> bool:
            nonlocal calls
            calls += 1
            if calls == 1:
                raise DatabaseError("INSERT", {}, CorruptError())
            return True

        monkeypatch.setattr(writer._repository, "insert", flaky_insert)
        monkeypatch.setattr(writer._store, "handle_corruption", lambda _exc: None)

        writer.write(_make_event())

        assert calls == 2

    def test_write_logs_busy_insert_loss(
        self,
        db_path: str,
        caplog: pytest.LogCaptureFixture,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        event = _make_event(event_type="busy_insert")
        writer = SqliteEventWriter(path=db_path)

        def raise_busy(_event: SecurityEvent) -> bool:
            raise _busy_database_error()

        monkeypatch.setattr(writer._repository, "insert", raise_busy)

        with caplog.at_level(logging.WARNING, logger=SQLITE_WRITER_LOGGER):
            writer.write(event)

        matching = [
            record
            for record in caplog.records
            if record.name == SQLITE_WRITER_LOGGER
            and record.message == "sqlite busy dropped security event"
        ]
        assert len(matching) == 1
        record = matching[0]
        # Domain fields live under `data` in the new schema; trace_id stays
        # top-level via the correlation slot.
        assert record.data["phase"] == "insert"
        assert record.data["event_id"] == event.event_id
        assert record.data["event_type"] == "busy_insert"
        assert record.data["category"] == event.category
        assert record.data["error_type"] == "BusyError"

    def test_write_logs_busy_session_factory_loss(
        self,
        db_path: str,
        caplog: pytest.LogCaptureFixture,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        event = _make_event(event_type="busy_session_factory")
        writer = SqliteEventWriter(path=db_path)

        def raise_locked(_db_identity: tuple[int, int] | None) -> None:
            raise _locked_database_error()

        monkeypatch.setattr(writer._store, "_open_session_factory", raise_locked)

        with caplog.at_level(logging.WARNING, logger=SQLITE_WRITER_LOGGER):
            writer.write(event)

        matching = [
            record
            for record in caplog.records
            if record.name == SQLITE_WRITER_LOGGER
            and record.message == "sqlite busy dropped security event"
        ]
        assert len(matching) == 1
        record = matching[0]
        assert record.data["phase"] == "insert"
        assert record.data["event_id"] == event.event_id
        assert record.data["event_type"] == "busy_session_factory"
        assert record.data["error_type"] == "LockedError"

    def test_write_logs_busy_corruption_retry_loss(
        self,
        db_path: str,
        caplog: pytest.LogCaptureFixture,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        class CorruptError(Exception):
            sqlite_errorcode = sqlite3.SQLITE_CORRUPT

        event = _make_event(event_type="busy_corruption_retry")
        writer = SqliteEventWriter(path=db_path)
        calls = 0
        dispose_calls: list[None] = []

        def corrupt_then_busy(_event: SecurityEvent) -> bool:
            nonlocal calls
            calls += 1
            if calls == 1:
                raise DatabaseError("INSERT", {}, CorruptError())
            raise _busy_database_error()

        def _track_dispose() -> None:
            dispose_calls.append(None)

        monkeypatch.setattr(writer._repository, "insert", corrupt_then_busy)
        monkeypatch.setattr(writer._store, "handle_corruption", lambda _exc: None)
        monkeypatch.setattr(writer._store, "dispose", _track_dispose)

        with caplog.at_level(logging.WARNING, logger=SQLITE_WRITER_LOGGER):
            writer.write(event)

        matching = [
            record
            for record in caplog.records
            if record.name == SQLITE_WRITER_LOGGER
            and record.message == "sqlite busy dropped security event"
        ]
        assert len(matching) == 1
        record = matching[0]
        assert record.data["phase"] == "corruption_retry"
        assert record.data["event_id"] == event.event_id
        assert record.data["event_type"] == "busy_corruption_retry"
        assert record.data["error_type"] == "BusyError"
        assert dispose_calls == []

    def test_write_disposes_on_corruption_retry_error(
        self, db_path: str, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        class CorruptError(Exception):
            sqlite_errorcode = sqlite3.SQLITE_CORRUPT

        event = _make_event(event_type="corruption_retry_error")
        writer = SqliteEventWriter(path=db_path)
        calls = 0
        dispose_calls: list[None] = []

        def corrupt_then_sqlalchemy(_event: SecurityEvent) -> bool:
            nonlocal calls
            calls += 1
            if calls == 1:
                raise DatabaseError("INSERT", {}, CorruptError())
            raise SQLAlchemyError("retry driver fault")

        def _track_dispose() -> None:
            dispose_calls.append(None)

        monkeypatch.setattr(writer._repository, "insert", corrupt_then_sqlalchemy)
        monkeypatch.setattr(writer._store, "handle_corruption", lambda _exc: None)
        monkeypatch.setattr(writer._store, "dispose", _track_dispose)

        writer.write(event)

        assert calls == 2
        assert len(dispose_calls) == 1

    def test_write_disposes_on_sqlalchemy_error(
        self, db_path: str, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        writer = SqliteEventWriter(path=db_path)
        disposed = False

        def raise_sqlalchemy_error(_event: SecurityEvent) -> bool:
            raise SQLAlchemyError("boom")

        def mark_disposed() -> None:
            nonlocal disposed
            disposed = True

        monkeypatch.setattr(writer._repository, "insert", raise_sqlalchemy_error)
        monkeypatch.setattr(writer._store, "dispose", mark_disposed)

        writer.write(_make_event())

        assert disposed
