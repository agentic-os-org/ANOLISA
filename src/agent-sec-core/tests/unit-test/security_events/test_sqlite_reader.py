"""Unit tests for security_events.sqlite_reader — SqliteEventReader."""

import io
import sqlite3
import sys
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

import pytest
from agent_sec_cli.security_events.schema import SecurityEvent, extract_verdict
from agent_sec_cli.security_events.sqlite_reader import SqliteEventReader
from agent_sec_cli.security_events.sqlite_writer import SqliteEventWriter
from agent_sec_cli.security_middleware.backends.skill_ledger import (
    SkillLedgerBackend,
)
from agent_sec_cli.security_middleware.result import ActionResult

CORRELATION_BASE_EPOCH = 1_800_000_000.0


def _make_event(
    event_type: str = "test_event", category: str = "test", **kwargs: Any
) -> SecurityEvent:
    return SecurityEvent(
        event_type=event_type,
        category=category,
        details=kwargs.get("details", {"key": "value"}),
        trace_id=kwargs.get("trace_id", ""),
    )


def _make_correlated_event(
    *,
    event_id: str,
    category: str,
    timestamp_epoch: float,
    session_id: str,
    run_id: str | None = None,
    tool_call_id: str | None = None,
) -> SecurityEvent:
    return SecurityEvent(
        event_id=event_id,
        event_type=f"{category}_event",
        category=category,
        timestamp=datetime.fromtimestamp(timestamp_epoch, timezone.utc).isoformat(),
        trace_id="trace",
        session_id=session_id,
        run_id=run_id,
        tool_call_id=tool_call_id,
        details={"event_id": event_id},
    )


@pytest.fixture()
def db_path(tmp_path: Path) -> str:
    return str(tmp_path / "test.db")


@pytest.fixture()
def tilde_db_path(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> str:
    monkeypatch.setenv("HOME", str(tmp_path))
    return "~/events.db"


@pytest.fixture()
def writer(db_path: str) -> SqliteEventWriter:
    # max_age_days=None disables retention prune so hardcoded-timestamp tests
    # don't depend on the wall clock at run time.
    w = SqliteEventWriter(path=db_path, max_age_days=None)
    yield w
    w.close()


@pytest.fixture()
def reader(db_path: str) -> SqliteEventReader:
    return SqliteEventReader(path=db_path)


class TestSqliteEventReader:
    def test_write_read_roundtrip(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        """Verify that a complete SecurityEvent can be written and read back with all fields intact.

        This is the most critical data path test — validates Writer → SQLite → Reader
        entire pipeline including _event_params, INSERT, SELECT, and _row_to_event.
        """
        # Create a comprehensive event with all fields
        original_event = SecurityEvent(
            event_type="harden",
            category="hardening",
            result="failed",
            timestamp="2026-04-20T13:47:00.123456+00:00",
            trace_id="test-trace-456",
            session_id="session-xyz",
            run_id="run-xyz",
            call_id="call-xyz",
            tool_call_id="tool-xyz",
            details={
                "request": {"config": "default", "dry_run": True},
                "result": {"violations": ["RULE_001", "RULE_002"]},
                "unicode": "测试中文🎉",
                "nested": {"level1": {"level2": "value"}},
                "list_data": [1, "two", 3.0, None],
                "empty_string": "",
                "null_value": None,
            },
        )

        # Write the event
        writer.write(original_event)
        writer.close()

        # Read it back
        events = reader.query(event_type="harden")
        assert len(events) == 1

        retrieved_event = events[0]

        # Verify all fields match
        assert retrieved_event.event_id == original_event.event_id
        assert retrieved_event.event_type == original_event.event_type
        assert retrieved_event.category == original_event.category
        assert retrieved_event.result == original_event.result
        assert retrieved_event.timestamp == original_event.timestamp
        assert retrieved_event.trace_id == original_event.trace_id
        assert retrieved_event.pid == original_event.pid
        assert retrieved_event.uid == original_event.uid
        assert retrieved_event.session_id == original_event.session_id
        assert retrieved_event.run_id == original_event.run_id
        assert retrieved_event.call_id == original_event.call_id
        assert retrieved_event.tool_call_id == original_event.tool_call_id

        # Verify details JSON round-trip
        assert retrieved_event.details == original_event.details
        assert retrieved_event.details["unicode"] == "测试中文🎉"
        assert retrieved_event.details["nested"]["level1"]["level2"] == "value"
        assert retrieved_event.details["list_data"] == [1, "two", 3.0, None]
        assert retrieved_event.details["null_value"] is None

    def test_malformed_details_are_skipped(
        self, db_path: str, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        """Verify that rows with malformed JSON in details are skipped gracefully."""
        # Write a valid event first
        writer.write(_make_event(event_type="valid_event"))

        # Manually insert a row with invalid JSON in details
        conn = sqlite3.connect(db_path)
        conn.execute(
            "INSERT INTO security_events "
            "(event_id, event_type, category, result, timestamp, timestamp_epoch, "
            "trace_id, pid, uid, session_id, details) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (
                "malformed-event-id",
                "malformed_event",
                "test",
                "succeeded",
                "2026-04-20T13:47:00+00:00",
                1745298420.0,
                "",
                12345,
                1000,
                None,
                "NOT VALID JSON{{{",  # Intentionally malformed
            ),
        )
        conn.commit()
        conn.close()

        # Capture stderr to verify warning is printed
        old_stderr = sys.stderr
        sys.stderr = io.StringIO()

        try:
            # Query should return only the valid event, skipping the malformed one
            events = reader.query()
            stderr_output = sys.stderr.getvalue()
        finally:
            sys.stderr = old_stderr

        # Should have skipped the malformed row
        assert len(events) == 1
        assert events[0].event_type == "valid_event"

        # Should have printed warning to stderr
        assert "malformed row skipped" in stderr_output

    def test_query_returns_all_events(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        for _ in range(5):
            writer.write(_make_event())
        events = reader.query()
        assert len(events) == 5

    def test_query_filter_by_event_type(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        writer.write(_make_event(event_type="alpha"))
        writer.write(_make_event(event_type="alpha"))
        writer.write(_make_event(event_type="beta"))
        events = reader.query(event_type="alpha")
        assert len(events) == 2
        for e in events:
            assert e.event_type == "alpha"

    def test_query_filter_by_category(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        writer.write(_make_event(category="sandbox"))
        writer.write(_make_event(category="sandbox"))
        writer.write(_make_event(category="hardening"))
        events = reader.query(category="sandbox")
        assert len(events) == 2
        for e in events:
            assert e.category == "sandbox"

    def test_query_filter_by_trace_id(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        writer.write(_make_event(trace_id="trace-abc"))
        writer.write(_make_event(trace_id="trace-abc"))
        writer.write(_make_event(trace_id="trace-xyz"))
        events = reader.query(trace_id="trace-abc")
        assert len(events) == 2
        for e in events:
            assert e.trace_id == "trace-abc"

    def test_query_time_range_since_until(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        now = datetime.now(timezone.utc)
        past = now - timedelta(hours=2)
        future = now + timedelta(hours=2)

        for _ in range(3):
            writer.write(_make_event())

        since_iso = past.isoformat()
        until_iso = future.isoformat()
        events = reader.query(since=since_iso, until=until_iso)
        assert len(events) == 3

    def test_query_ordering_desc(
        self, db_path: str, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        writer.write(_make_event())
        time.sleep(0.02)
        writer.write(_make_event())
        time.sleep(0.02)
        writer.write(_make_event())

        events = reader.query()
        assert len(events) == 3
        # Results should be in descending order by time — verify via DB directly
        conn = sqlite3.connect(db_path)
        rows = conn.execute(
            "SELECT timestamp_epoch FROM security_events ORDER BY timestamp_epoch DESC"
        ).fetchall()
        conn.close()
        epochs = [r[0] for r in rows]
        assert epochs == sorted(epochs, reverse=True)

    def test_query_limit_offset(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        for _ in range(10):
            writer.write(_make_event())
            time.sleep(0.005)

        events = reader.query(limit=3, offset=2)
        assert len(events) == 3

    def test_count_returns_total(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        for _ in range(5):
            writer.write(_make_event())
        assert reader.count() == 5

    def test_count_with_filters(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        writer.write(_make_event(category="sandbox"))
        writer.write(_make_event(category="sandbox"))
        writer.write(_make_event(category="hardening"))
        assert reader.count(category="sandbox") == 2

    def test_count_filter_by_trace_id(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        writer.write(_make_event(trace_id="trace-abc"))
        writer.write(_make_event(trace_id="trace-abc"))
        writer.write(_make_event(trace_id="trace-xyz"))

        assert reader.count(trace_id="trace-abc") == 2

    def test_count_respects_offset(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        base = datetime(2026, 1, 1, tzinfo=timezone.utc)
        for idx, category in enumerate(("alpha", "beta", "alpha")):
            writer.write(
                SecurityEvent(
                    event_type="scan",
                    category=category,
                    timestamp=(base + timedelta(seconds=idx)).isoformat(),
                    trace_id=f"trace-{idx}",
                    details={},
                )
            )

        assert reader.count(offset=1) == 2
        assert reader.count(category="alpha", offset=1) == 1

    def test_count_by_respects_offset(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        base = datetime(2026, 1, 1, tzinfo=timezone.utc)
        for idx, category in enumerate(("alpha", "beta", "alpha")):
            writer.write(
                SecurityEvent(
                    event_type="scan",
                    category=category,
                    timestamp=(base + timedelta(seconds=idx)).isoformat(),
                    trace_id=f"trace-{idx}",
                    details={},
                )
            )

        assert reader.count_by("category", offset=1) == {"alpha": 1, "beta": 1}

    def test_summary_returns_aggregates_and_latest_events(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        writer.write(
            SecurityEvent(
                event_id="older-deny",
                event_type="scan",
                category="code_scan",
                timestamp="2026-05-19T00:00:00+00:00",
                session_id="session-1",
                run_id="run-1",
                details={"verdict": "deny"},
            )
        )
        writer.write(
            SecurityEvent(
                event_id="pass-event",
                event_type="scan",
                category="code_scan",
                timestamp="2026-05-19T00:01:00+00:00",
                session_id="session-1",
                run_id="run-1",
                details={"verdict": "pass"},
            )
        )
        writer.write(
            SecurityEvent(
                event_id="newer-deny",
                event_type="prompt",
                category="prompt_scan",
                timestamp="2026-05-19T00:02:00+00:00",
                session_id="session-2",
                run_id="run-2",
                details={"verdict": "deny"},
            )
        )

        summary = reader.summary(verdict="deny", latest_limit=1)

        assert summary.total == 2
        assert summary.by_category == {"code_scan": 1, "prompt_scan": 1}
        assert summary.by_event_type == {"scan": 1, "prompt": 1}
        assert summary.by_result == {"succeeded": 2}
        assert summary.by_session == {"session-1": 1, "session-2": 1}
        assert summary.by_run == {"run-1": 1, "run-2": 1}
        assert [event.event_id for event in summary.latest_events] == ["newer-deny"]

    def test_query_rejects_naive_time_range_at_repository_boundary(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        writer.write(
            SecurityEvent(
                event_type="scan",
                category="prompt_scan",
                timestamp=datetime(2026, 5, 20, 4, 0, tzinfo=timezone.utc).isoformat(),
                trace_id="local-match",
                details={},
            )
        )

        with pytest.raises(ValueError, match="timezone information"):
            reader.query(
                since="2026-05-20T11:59:00",
                until="2026-05-20T12:01:00",
            )

    def test_query_count_and_count_by_support_dashboard_filters(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        in_window = datetime(2026, 5, 20, 12, 0, tzinfo=timezone.utc)
        old = in_window - timedelta(days=1)
        writer.write(
            SecurityEvent(
                event_type="code_scan",
                category="code_scan",
                result="failed",
                timestamp=in_window.isoformat(),
                trace_id="trace-1",
                session_id="session-1",
                run_id="run-1",
                call_id="call-1",
                tool_call_id="tool-1",
                details={"request": {}, "result": {"verdict": "deny"}},
            )
        )
        writer.write(
            SecurityEvent(
                event_type="prompt_scan",
                category="prompt_scan",
                result="succeeded",
                timestamp=in_window.isoformat(),
                trace_id="trace-2",
                session_id="session-1",
                run_id="run-2",
                call_id="call-2",
                tool_call_id="tool-2",
                details={},
            )
        )
        writer.write(
            SecurityEvent(
                event_type="code_scan",
                category="code_scan",
                result="failed",
                timestamp=old.isoformat(),
                trace_id="trace-old",
                session_id="session-1",
                run_id="run-old",
                call_id="call-old",
                tool_call_id="tool-old",
                details={},
            )
        )

        events = reader.query(
            result="failed",
            session_id="session-1",
            run_id="run-1",
            call_id="call-1",
            tool_call_id="tool-1",
            since=(in_window - timedelta(seconds=1)).isoformat(),
            until=(in_window + timedelta(seconds=1)).isoformat(),
        )

        assert [event.trace_id for event in events] == ["trace-1"]
        assert (
            reader.count(
                session_id="session-1",
                since=(in_window - timedelta(seconds=1)).isoformat(),
                until=(in_window + timedelta(seconds=1)).isoformat(),
            )
            == 2
        )
        assert reader.count_by(
            "result",
            session_id="session-1",
            since=(in_window - timedelta(seconds=1)).isoformat(),
            until=(in_window + timedelta(seconds=1)).isoformat(),
        ) == {"failed": 1, "succeeded": 1}
        assert reader.count_by(
            "run_id",
            session_id="session-1",
            since=(in_window - timedelta(seconds=1)).isoformat(),
            until=(in_window + timedelta(seconds=1)).isoformat(),
        ) == {"run-1": 1, "run-2": 1}

    def test_count_by_category(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        writer.write(_make_event(category="sandbox"))
        writer.write(_make_event(category="sandbox"))
        writer.write(_make_event(category="hardening"))
        result = reader.count_by("category")
        assert result["sandbox"] == 2
        assert result["hardening"] == 1

    def test_count_by_event_type(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        writer.write(_make_event(event_type="alpha"))
        writer.write(_make_event(event_type="alpha"))
        writer.write(_make_event(event_type="beta"))
        result = reader.count_by("event_type")
        assert result["alpha"] == 2
        assert result["beta"] == 1

    def test_count_by_filter_by_trace_id(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        writer.write(_make_event(category="sandbox", trace_id="trace-abc"))
        writer.write(_make_event(category="hardening", trace_id="trace-abc"))
        writer.write(_make_event(category="sandbox", trace_id="trace-xyz"))

        result = reader.count_by("category", trace_id="trace-abc")

        assert result == {"hardening": 1, "sandbox": 1}

    def test_count_by_filter_by_event_type_and_category(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        writer.write(
            _make_event(event_type="alpha", category="sandbox", trace_id="trace-1")
        )
        writer.write(
            _make_event(event_type="alpha", category="hardening", trace_id="trace-2")
        )
        writer.write(
            _make_event(event_type="beta", category="sandbox", trace_id="trace-3")
        )

        result = reader.count_by("trace_id", event_type="alpha", category="sandbox")

        assert result == {"trace-1": 1}

    def test_count_by_invalid_field_raises(self, reader: SqliteEventReader) -> None:
        with pytest.raises(ValueError):
            reader.count_by("invalid_field")

    def test_missing_db_returns_empty(self, tmp_path: Path) -> None:
        missing_path = str(tmp_path / "nonexistent.db")
        reader = SqliteEventReader(path=missing_path)
        assert reader.query() == []
        assert reader.count() == 0
        assert reader.count_by("category") == {}

    def test_reader_reopens_after_db_file_replaced(self, db_path: str) -> None:
        writer = SqliteEventWriter(path=db_path)
        writer.write(_make_event(event_type="before_replace"))
        writer.close()

        reader = SqliteEventReader(path=db_path)
        assert [event.event_type for event in reader.query()] == ["before_replace"]

        db = Path(db_path)
        db.unlink()
        Path(f"{db_path}-wal").unlink(missing_ok=True)
        Path(f"{db_path}-shm").unlink(missing_ok=True)

        writer = SqliteEventWriter(path=db_path)
        writer.write(_make_event(event_type="after_replace"))
        writer.close()

        assert [event.event_type for event in reader.query()] == ["after_replace"]

    def test_reader_recovers_after_schema_created_in_existing_db(
        self, db_path: str
    ) -> None:
        conn = sqlite3.connect(db_path)
        conn.close()

        reader = SqliteEventReader(path=db_path)
        assert reader.query() == []

        writer = SqliteEventWriter(path=db_path)
        writer.write(_make_event(event_type="after_schema"))
        writer.close()

        assert [event.event_type for event in reader.query()] == ["after_schema"]

    def test_tilde_path_is_normalized_for_reader_and_writer(
        self, tilde_db_path: str
    ) -> None:
        writer = SqliteEventWriter(path=tilde_db_path)
        writer.write(_make_event(event_type="tilde_path"))
        writer.close()

        reader = SqliteEventReader(path=tilde_db_path)
        assert [event.event_type for event in reader.query()] == ["tilde_path"]
        assert not Path("~").exists()

    def test_compatibility_helpers_delegate_to_store(self, db_path: str) -> None:
        writer = SqliteEventWriter(path=db_path)
        writer.write(_make_event(event_type="compat"))
        writer.close()

        reader = SqliteEventReader(path=db_path)
        assert reader._ensure_session_factory() is not None
        assert reader._engine is not None
        assert reader._session_factory is not None

        reader._dispose_engine()
        assert reader._engine is None
        assert reader._session_factory is None

    def test_round_trips_new_tracing_fields(self, db_path: str) -> None:
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

        events = SqliteEventReader(path=db_path).query(limit=10)

        assert len(events) == 1
        assert events[0].trace_id == "trace-1"
        assert events[0].session_id == "session-1"
        assert events[0].run_id == "run-1"
        assert events[0].call_id == "call-1"
        assert events[0].tool_call_id == "tool-1"

    def test_read_only_v1_schema_missing_new_columns_warns_and_returns_empty(
        self,
        db_path: str,
        capsys: pytest.CaptureFixture[str],
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
                'old-trace', 1, 1, 'old-session', '{}'
            )
            """)
        conn.commit()
        conn.close()

        assert SqliteEventReader(path=db_path).query(limit=10) == []
        stderr = capsys.readouterr().err
        assert "sqlite schema is v1, this binary expects v3" in stderr
        assert "run any write command" in stderr
        assert "read-only queries may return empty results until then" in stderr

        conn = sqlite3.connect(db_path)
        try:
            user_version = conn.execute("PRAGMA user_version").fetchone()[0]
            columns = {
                row[1] for row in conn.execute("PRAGMA table_info(security_events)")
            }
        finally:
            conn.close()

        assert user_version == 1
        assert "run_id" not in columns

    def test_close_disposes_readonly_store(self, db_path: str) -> None:
        writer = SqliteEventWriter(path=db_path)
        writer.write(_make_event(event_type="close"))
        writer.close()

        reader = SqliteEventReader(path=db_path)
        assert reader.query()

        reader.close()
        assert reader._engine is None
        assert reader._session_factory is None


class TestCorrelationCandidateQuery:
    def test_candidates_match_session_run_tool_call_and_category(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        writer.write(
            _make_correlated_event(
                event_id="match-code",
                category="code_scan",
                timestamp_epoch=CORRELATION_BASE_EPOCH,
                session_id="session-1",
                run_id="run-1",
                tool_call_id="tool-1",
            )
        )
        writer.write(
            _make_correlated_event(
                event_id="match-skill",
                category="skill_ledger",
                timestamp_epoch=CORRELATION_BASE_EPOCH + 1.0,
                session_id="session-1",
                run_id="run-1",
                tool_call_id="tool-1",
            )
        )
        writer.write(
            _make_correlated_event(
                event_id="wrong-tool",
                category="code_scan",
                timestamp_epoch=CORRELATION_BASE_EPOCH + 2.0,
                session_id="session-1",
                run_id="run-1",
                tool_call_id="tool-2",
            )
        )
        writer.write(
            _make_correlated_event(
                event_id="wrong-run",
                category="code_scan",
                timestamp_epoch=CORRELATION_BASE_EPOCH + 3.0,
                session_id="session-1",
                run_id="run-2",
                tool_call_id="tool-1",
            )
        )
        writer.write(
            _make_correlated_event(
                event_id="wrong-session",
                category="code_scan",
                timestamp_epoch=CORRELATION_BASE_EPOCH + 4.0,
                session_id="session-2",
                run_id="run-1",
                tool_call_id="tool-1",
            )
        )
        writer.write(
            _make_correlated_event(
                event_id="wrong-category",
                category="sandbox",
                timestamp_epoch=CORRELATION_BASE_EPOCH + 5.0,
                session_id="session-1",
                run_id="run-1",
                tool_call_id="tool-1",
            )
        )
        writer.close()

        candidates = reader.query_correlation_candidates(
            session_id="session-1",
            categories=("code_scan", "skill_ledger"),
            run_id="run-1",
            tool_call_id="tool-1",
        )

        assert [candidate.event.event_id for candidate in candidates] == [
            "match-code",
            "match-skill",
        ]
        assert [candidate.timestamp_epoch for candidate in candidates] == [
            CORRELATION_BASE_EPOCH,
            CORRELATION_BASE_EPOCH + 1.0,
        ]

    def test_candidates_filter_inclusive_epoch_window(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        for event_id, timestamp_epoch in (
            ("too-early", 997.99),
            ("lower-bound", 998.0),
            ("center", 1000.0),
            ("upper-bound", 1002.0),
            ("too-late", 1002.01),
        ):
            writer.write(
                _make_correlated_event(
                    event_id=event_id,
                    category="prompt_scan",
                    timestamp_epoch=CORRELATION_BASE_EPOCH + timestamp_epoch,
                    session_id="session-1",
                    run_id="run-1",
                )
            )
        writer.close()

        candidates = reader.query_correlation_candidates(
            session_id="session-1",
            categories=["prompt_scan"],
            run_id="run-1",
            since_epoch=CORRELATION_BASE_EPOCH + 998.0,
            until_epoch=CORRELATION_BASE_EPOCH + 1002.0,
        )

        assert [candidate.event.event_id for candidate in candidates] == [
            "lower-bound",
            "center",
            "upper-bound",
        ]

    def test_candidates_filter_multiple_tool_call_ids(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        for event_id, tool_call_id in (
            ("tool-1-match", "tool-1"),
            ("tool-2-match", "tool-2"),
            ("tool-3-skip", "tool-3"),
        ):
            writer.write(
                _make_correlated_event(
                    event_id=event_id,
                    category="code_scan",
                    timestamp_epoch=CORRELATION_BASE_EPOCH,
                    session_id="session-1",
                    run_id="run-1",
                    tool_call_id=tool_call_id,
                )
            )
        writer.close()

        candidates = reader.query_correlation_candidates(
            session_id="session-1",
            categories=("code_scan",),
            run_id="run-1",
            tool_call_ids=("tool-1", "tool-2"),
        )

        assert [candidate.event.event_id for candidate in candidates] == [
            "tool-1-match",
            "tool-2-match",
        ]

    def test_candidates_are_limited_to_1000_rows(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        for index in range(1001):
            writer.write(
                _make_correlated_event(
                    event_id=f"candidate-{index:04d}",
                    category="code_scan",
                    timestamp_epoch=CORRELATION_BASE_EPOCH + index,
                    session_id="session-1",
                    run_id="run-1",
                    tool_call_id="tool-1",
                )
            )
        writer.close()

        candidates = reader.query_correlation_candidates(
            session_id="session-1",
            categories=("code_scan",),
            run_id="run-1",
            tool_call_id="tool-1",
        )

        assert len(candidates) == 1000
        assert candidates[0].event.event_id == "candidate-0000"
        assert candidates[-1].event.event_id == "candidate-0999"

    def test_candidates_do_not_filter_run_when_run_id_omitted(
        self, writer: SqliteEventWriter, reader: SqliteEventReader
    ) -> None:
        writer.write(
            _make_correlated_event(
                event_id="run-1-match",
                category="pii_scan",
                timestamp_epoch=CORRELATION_BASE_EPOCH,
                session_id="session-1",
                run_id="run-1",
            )
        )
        writer.write(
            _make_correlated_event(
                event_id="run-2-match",
                category="pii_scan",
                timestamp_epoch=CORRELATION_BASE_EPOCH + 1.0,
                session_id="session-1",
                run_id="run-2",
            )
        )
        writer.write(
            _make_correlated_event(
                event_id="wrong-session",
                category="pii_scan",
                timestamp_epoch=CORRELATION_BASE_EPOCH + 2.0,
                session_id="session-2",
                run_id="run-1",
            )
        )
        writer.close()

        candidates = reader.query_correlation_candidates(
            session_id="session-1",
            categories=("pii_scan",),
            since_epoch=CORRELATION_BASE_EPOCH - 1.0,
            until_epoch=CORRELATION_BASE_EPOCH + 2.0,
        )

        assert [candidate.event.event_id for candidate in candidates] == [
            "run-1-match",
            "run-2-match",
        ]

    def test_candidates_return_empty_when_schema_unavailable(
        self, tmp_path: Path
    ) -> None:
        reader = SqliteEventReader(path=str(tmp_path / "missing.db"))

        assert (
            reader.query_correlation_candidates(
                session_id="session-1",
                categories=("code_scan",),
            )
            == []
        )


class TestVerdictSqlFilter:
    """Tests for SQL-backed verdict filtering."""

    @pytest.fixture(autouse=True)
    def _seed_events(self, writer: SqliteEventWriter) -> None:
        """Seed events with varying verdicts."""
        for idx, (verdict, category) in enumerate(
            [
                ("pass", "prompt_scan"),
                ("deny", "prompt_scan"),
                ("warn", "code_scan"),
                ("pass", "pii_scan"),
                (None, "skill_ledger"),  # no verdict
                ("deny", "prompt_scan"),
                ("deny", "code_scan"),
            ]
        ):
            details: dict[str, Any] = {}
            if verdict is not None:
                if category == "code_scan" and verdict == "deny":
                    details["result"] = {"verdict": verdict}
                else:
                    details["verdict"] = verdict
            writer.write(
                SecurityEvent(
                    event_type=f"{category}_event",
                    category=category,
                    details=details,
                    timestamp=(
                        datetime(2025, 1, 1, tzinfo=timezone.utc)
                        + timedelta(seconds=idx)
                    ).isoformat(),
                )
            )
        writer.close()

    def test_query_filter_by_verdict(self, reader: SqliteEventReader) -> None:
        events = reader.query(verdict="deny")
        assert len(events) == 3
        assert all(_details_verdict(e.details) == "deny" for e in events)

    def test_query_verdict_with_pagination(self, reader: SqliteEventReader) -> None:
        events = reader.query(verdict="pass", limit=1, offset=0)
        assert len(events) == 1
        assert _details_verdict(events[0].details) == "pass"

        events_page2 = reader.query(verdict="pass", limit=1, offset=1)
        assert len(events_page2) == 1

    def test_count_with_verdict_filter(self, reader: SqliteEventReader) -> None:
        assert reader.count(verdict="deny") == 3
        assert reader.count(verdict="deny", offset=1) == 2
        assert reader.count(verdict="pass") == 2
        assert reader.count(verdict="warn") == 1
        assert reader.count(verdict="nonexistent") == 0

    def test_count_by_verdict(self, reader: SqliteEventReader) -> None:
        groups = reader.count_by("verdict")
        assert groups == {"pass": 2, "deny": 3, "warn": 1}
        offset_groups = reader.count_by("verdict", offset=1)
        assert offset_groups == {"pass": 2, "deny": 2, "warn": 1}

    def test_count_by_verdict_with_category_filter(
        self, reader: SqliteEventReader
    ) -> None:
        groups = reader.count_by("verdict", category="prompt_scan")
        assert groups == {"pass": 1, "deny": 2}

    def test_count_by_category_with_verdict_filter(
        self, reader: SqliteEventReader
    ) -> None:
        groups = reader.count_by("category", verdict="deny")
        assert groups == {"prompt_scan": 2, "code_scan": 1}
        offset_groups = reader.count_by("category", verdict="deny", offset=1)
        assert offset_groups == {"prompt_scan": 2}

    def test_query_without_verdict_unchanged(self, reader: SqliteEventReader) -> None:
        """Ensure queries without verdict still work via the SQL path."""
        events = reader.query(limit=3)
        assert len(events) == 3
        total = reader.count()
        assert total == 7


def test_new_skill_ledger_events_populate_existing_verdict_index(
    writer: SqliteEventWriter,
    reader: SqliteEventReader,
    db_path: str,
) -> None:
    backend = SkillLedgerBackend()
    show_details = backend.build_event_details(
        ActionResult(
            success=True,
            data={
                "command": "show",
                "latestStatus": "deny",
                "skillName": "risky-skill",
                "latest": {"scanStatus": "deny"},
                "active": {"scanStatus": "pass"},
            },
        ),
        {"command": "show", "skill_dir": "/opt/skills/risky-skill"},
    )
    check_details = backend.build_event_details(
        ActionResult(
            success=False,
            data={
                "command": "check",
                "status": "tampered",
                "skillName": "tampered-skill",
            },
        ),
        {"command": "check", "skill_dir": "/opt/skills/tampered-skill"},
    )
    historical_details = {
        "request": {"command": "show", "skill_dir": "/opt/skills/old"},
        "result": {
            "command": "show",
            "latest_status": "deny",
            "latest": {"verdict": "deny"},
        },
    }

    for event_id, details in (
        ("new-show", show_details),
        ("new-check", check_details),
        ("historical-shape", historical_details),
    ):
        writer.write(
            SecurityEvent(
                event_id=event_id,
                event_type="skill_ledger",
                category="skill_ledger",
                details=details,
            )
        )

    assert [event.event_id for event in reader.query(verdict="deny")] == ["new-show"]
    assert [event.event_id for event in reader.query(verdict="tampered")] == [
        "new-check"
    ]
    assert reader.count_by("verdict", category="skill_ledger") == {
        "deny": 1,
        "tampered": 1,
    }

    with sqlite3.connect(db_path) as conn:
        historical_verdict = conn.execute(
            "SELECT verdict FROM security_events WHERE event_id = ?",
            ("historical-shape",),
        ).fetchone()
    assert historical_verdict == (None,)


def _details_verdict(details: dict[str, Any]) -> str | None:
    return extract_verdict(details)
