"""SQLite-backed regressions for complete per-session security totals."""

from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from sqlalchemy import event
from sqlalchemy.exc import OperationalError

from agent_sec_cli.observability.session_report import build_session_report, format_text
from agent_sec_cli.security_events.schema import SecurityEvent
from agent_sec_cli.security_events.sqlite_reader import SqliteEventReader
from agent_sec_cli.security_events.sqlite_writer import SqliteEventWriter


@pytest.mark.parametrize("successful_count", [999, 1000, 1001])
def test_session_report_counts_late_failures(tmp_path: Path, successful_count: int) -> None:
    path = tmp_path / "security.db"
    writer = SqliteEventWriter(path, max_age_days=None)
    reader = SqliteEventReader(path)
    obs_reader = SimpleNamespace(
        list_sessions=lambda: [
            SimpleNamespace(
                session_id="target", first_seen_epoch=0, last_seen_epoch=1, turn_count=1
            )
        ],
        list_runs=lambda _session_id: [],
    )
    try:
        for index in range(successful_count):
            writer.write(
                SecurityEvent(
                    event_id=f"success-{index}",
                    event_type="scan_code",
                    category="code_scan",
                    session_id="target",
                    timestamp="2026-10-01T00:00:00+00:00",
                    details={"verdict": "pass"},
                )
            )
        for event_id, session_id, category in [
            ("late-failure", "target", "code_scan"),
            ("other-session", "neighbor", "code_scan"),
            ("unsupported-category", "target", "other"),
            ("second-category", "target", "pii_scan"),
        ]:
            writer.write(
                SecurityEvent(
                    event_id=event_id,
                    event_type="scan",
                    category=category,
                    session_id=session_id,
                    timestamp="2026-10-01T00:01:00+00:00",
                    result="failed",
                    details={"verdict": "error"},
                )
            )
        assert reader.count(session_id="target") == successful_count + 3
        report = build_session_report("target", obs_reader, reader)
        assert report is not None
        assert report.security_verdicts == {
            "code_scan": {"succeeded": successful_count, "failed": 1},
            "pii_scan": {"failed": 1},
        }
        assert report.security_hint == ""
        assert "failed: 1" in format_text(report)
        assert report.to_dict()["security_verdicts"] == report.security_verdicts
        # The bounded lookup is still appropriate for correlating individual events.
        assert (
            len(
                reader.query_correlation_candidates(
                    session_id="target", categories=["code_scan", "pii_scan"]
                )
            )
            == 1000
        )
    finally:
        reader.close()
        writer.close()


@pytest.mark.parametrize("unavailable", ["missing", "query_failure"])
def test_unavailable_counts_cannot_report_a_clean_session(tmp_path: Path, unavailable: str) -> None:
    path = tmp_path / "security.db"
    writer = SqliteEventWriter(path, max_age_days=None)
    reader = SqliteEventReader(path)
    obs_reader = SimpleNamespace(
        list_sessions=lambda: [
            SimpleNamespace(
                session_id="target", first_seen_epoch=0, last_seen_epoch=1, turn_count=1
            )
        ],
        list_runs=lambda _session_id: [],
    )
    try:
        if unavailable == "query_failure":
            writer.write(
                SecurityEvent(
                    event_type="scan", category="code_scan", session_id="target", details={}
                )
            )
            assert reader.count(session_id="target") == 1

            def fail_query(
                conn: Any,
                cursor: Any,
                statement: str,
                parameters: Any,
                context: Any,
                executemany: bool,
            ) -> None:
                raise OperationalError(statement, parameters, RuntimeError("database is locked"))

            event.listen(reader._engine, "before_cursor_execute", fail_query)
        report = build_session_report("target", obs_reader, reader)
        assert report is not None
        assert report.security_verdicts == {}
        assert report.security_hint == "failed to query security events"
    finally:
        reader.close()
        writer.close()


def test_available_empty_session_has_no_failure_hint(tmp_path: Path) -> None:
    path = tmp_path / "security.db"
    writer = SqliteEventWriter(path, max_age_days=None)
    reader = SqliteEventReader(path)
    try:
        writer.write(
            SecurityEvent(event_type="scan", category="code_scan", session_id="other", details={})
        )
        assert reader.count_session_results("target", ["code_scan"]) == {}
        assert reader.count_session_results("other", []) == {}
    finally:
        reader.close()
        writer.close()
