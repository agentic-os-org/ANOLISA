"""E2E test: CLI capability invocation → event query pipeline.

Validates that invoking security capabilities through the CLI produces
queryable security events in the SQLite store.

NOTE: These tests verify the event-logging pipeline, not the security
capabilities themselves.  `harden` may exit 127 (loongshield missing),
`verify` may find zero skills — both are acceptable as long as an event
is recorded.

Isolation: Each test function uses its own dedicated temp directory (via
AGENT_SEC_DATA_DIR env var) so that tests are fully independent — no
shared state, no ordering dependency, no cascade failures.
"""

import json
import os
import sqlite3
import time
from datetime import datetime, timezone
from pathlib import Path

from cli.conftest import iso_now, require_loongshield, run_cli


def _run_harden_and_expected_event_result() -> str:
    """Run harden and return the expected SecurityEvent.result value."""
    result = run_cli("harden")
    return "succeeded" if result.returncode == 0 else "failed"


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


def _expected_event_result(cli_result):
    return "succeeded" if cli_result.returncode == 0 else "failed"


def _extract_verdict(details: dict) -> str | None:
    """Return verdict from event details without importing agent_sec_cli internals."""
    direct = details.get("verdict")
    if isinstance(direct, str):
        return direct

    nested_result = details.get("result")
    if isinstance(nested_result, dict):
        nested = nested_result.get("verdict")
        if isinstance(nested, str):
            return nested

    return None


def _write_security_event_row(
    *,
    event_id: str,
    event_type: str,
    category: str,
    result: str,
    trace_id: str,
    details: dict,
    session_id: str | None = None,
    run_id: str | None = None,
    timestamp: str = "2026-06-09T00:00:00+00:00",
    timestamp_epoch: float = 1780963200.0,
) -> None:
    data_dir = Path(os.environ["AGENT_SEC_DATA_DIR"])
    db_path = data_dir / "security-events.db"
    db_path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(db_path)
    try:
        conn.executescript("""
            PRAGMA user_version = 3;
            CREATE TABLE IF NOT EXISTS security_events (
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
                verdict TEXT,
                details TEXT NOT NULL
            );
            """)
        conn.execute(
            """
            INSERT INTO security_events (
                event_id,
                event_type,
                category,
                result,
                timestamp,
                timestamp_epoch,
                trace_id,
                pid,
                uid,
                session_id,
                run_id,
                call_id,
                tool_call_id,
                verdict,
                details
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (
                event_id,
                event_type,
                category,
                result,
                timestamp,
                timestamp_epoch,
                trace_id,
                os.getpid(),
                os.getuid(),
                session_id,
                run_id,
                None,
                None,
                _extract_verdict(details),
                json.dumps(details),
            ),
        )
        conn.commit()
    finally:
        conn.close()


class TestHardenEventLogging:
    """Verify that invoking `harden` produces a queryable event."""

    def test_harden_produces_event(self):
        """After `agent-sec-cli harden`, an event with event_type=harden is queryable."""
        since = iso_now()

        # Small delay to ensure timestamp ordering
        time.sleep(0.05)

        # Invoke harden — exit code doesn't matter (loongshield may be absent)
        run_cli("harden")

        # Small delay to let SQLite WAL flush
        time.sleep(0.1)

        # Query events since the start of this test
        result = run_cli(
            "events", "--event-type", "harden", "--since", since, "--output", "json"
        )
        assert result.returncode == 0, f"events query failed: {result.stderr}"

        events = json.loads(result.stdout)
        assert isinstance(events, list)
        assert (
            len(events) == 1
        ), f"Expected exactly 1 harden event since {since}, got {len(events)}"

        # Verify event structure
        event = events[0]
        assert event["event_type"] == "harden"
        assert event["category"] == "hardening"
        assert "event_id" in event
        assert "timestamp" in event
        assert "details" in event

    def test_harden_event_count(self):
        """--count returns exactly 1 after a single harden invocation."""
        since = iso_now()
        time.sleep(0.05)

        run_cli("harden")
        time.sleep(0.1)

        result = run_cli(
            "events", "--count", "--event-type", "harden", "--since", since
        )
        assert result.returncode == 0
        count = json.loads(result.stdout)
        assert count == 1


class TestVerifyEventLogging:
    """Verify that invoking `verify` produces a queryable event."""

    def test_verify_produces_event(self):
        """After `agent-sec-cli verify`, an event with event_type=verify is queryable."""
        since = iso_now()
        time.sleep(0.05)

        # Invoke verify — may fail (no skills configured), that's acceptable
        run_cli("verify")
        time.sleep(0.1)

        # Query events
        result = run_cli(
            "events", "--event-type", "verify", "--since", since, "--output", "json"
        )
        assert result.returncode == 0

        events = json.loads(result.stdout)
        assert isinstance(events, list)
        assert (
            len(events) == 1
        ), f"Expected exactly 1 verify event since {since}, got {len(events)}"

        event = events[0]
        assert event["event_type"] == "verify"
        assert event["category"] == "asset_verify"
        assert "details" in event

    def test_verify_event_count_by_category(self):
        """--count-by category shows asset_verify: 1 after a single verify invocation."""
        since = iso_now()
        time.sleep(0.05)

        run_cli("verify")
        time.sleep(0.1)

        result = run_cli("events", "--count-by", "category", "--since", since)
        assert result.returncode == 0

        counts = json.loads(result.stdout)
        assert isinstance(counts, dict)
        assert counts == {"asset_verify": 1}


class TestEventQueryFilters:
    """Verify that query filters work end-to-end."""

    def test_last_hours_filter(self):
        """--last-hours returns exactly the single event just created."""
        run_cli("harden")
        time.sleep(0.1)

        # Fresh DB: only this test's event exists.
        result = run_cli(
            "events", "--event-type", "harden", "--last-hours", "1", "--output", "json"
        )
        assert result.returncode == 0
        events = json.loads(result.stdout)
        assert len(events) == 1

    def test_nonexistent_type_returns_empty(self):
        """Filtering by a non-existent event_type returns empty list."""
        result = run_cli(
            "events",
            "--event-type",
            "does_not_exist_xyz",
            "--last-hours",
            "1",
            "--output",
            "json",
        )
        assert result.returncode == 0

        events = json.loads(result.stdout)
        assert events == []

    def test_default_table_output(self):
        """Default output is human-readable table format."""
        since = iso_now()
        time.sleep(0.05)
        harden_result = run_cli("harden")
        expected_result = _expected_event_result(harden_result)
        time.sleep(0.1)

        result = run_cli("events", "--event-type", "harden", "--since", since)
        assert result.returncode == 0
        # Default output is table — should NOT be parseable as JSON
        lines = result.stdout.strip().split("\n")
        # Header + 1 data row + blank line + footer
        assert len(lines) == 4
        assert lines[0].startswith("EVENT_TYPE")
        assert "harden" in lines[1]
        assert expected_result in lines[1]
        assert "1 event" in lines[3]


class TestCLIValidation:
    """Verify CLI parameter validation and error handling."""

    def test_invalid_output_format(self):
        """Verify that invalid --output format returns error."""
        result = run_cli("events", "--output", "xml")
        assert result.returncode == 1
        assert "Error:" in result.stderr
        assert "--output must be one of" in result.stderr

    def test_last_hours_and_since_mutual_exclusion(self):
        """Verify that --last-hours and --since are mutually exclusive."""
        result = run_cli(
            "events",
            "--last-hours",
            "24",
            "--since",
            "2026-01-01T00:00:00Z",
        )
        assert result.returncode == 1
        assert "Error:" in result.stderr
        assert "mutually exclusive" in result.stderr

    def test_count_and_count_by_mutual_exclusion(self):
        """Verify that --count and --count-by are mutually exclusive."""
        result = run_cli("events", "--count", "--count-by", "category")
        assert result.returncode == 1
        assert "Error:" in result.stderr
        assert "mutually exclusive" in result.stderr

    def test_invalid_count_by_field(self):
        """Verify that invalid --count-by field returns error."""
        result = run_cli("events", "--count-by", "invalid_field")
        assert result.returncode == 1
        assert "Error:" in result.stderr
        assert "--count-by must be one of" in result.stderr

    def test_unknown_event_type_warning(self):
        """Verify that unknown event_type produces a warning but doesn't fail."""
        result = run_cli(
            "events",
            "--event-type",
            "unknown_type",
            "--last-hours",
            "1",
            "--output",
            "json",
        )
        # Should succeed (exit 0) but print warning to stderr
        assert result.returncode == 0
        assert "Warning:" in result.stderr
        assert "Unknown event_type" in result.stderr

    def test_unknown_category_warning(self):
        """Verify that unknown category produces a warning but doesn't fail."""
        result = run_cli(
            "events",
            "--category",
            "unknown_category",
            "--last-hours",
            "1",
            "--output",
            "json",
        )
        # Should succeed (exit 0) but print warning to stderr
        assert result.returncode == 0
        assert "Warning:" in result.stderr
        assert "Unknown category" in result.stderr

    def test_json_output_format(self):
        """Verify that --output json returns a valid JSON array with complete event data."""
        since = iso_now()
        time.sleep(0.05)
        harden_result = run_cli("harden")
        expected_result = _expected_event_result(harden_result)
        time.sleep(0.1)

        result = run_cli(
            "events", "--event-type", "harden", "--since", since, "--output", "json"
        )
        assert result.returncode == 0

        # Should be valid JSON array
        events = json.loads(result.stdout)
        assert isinstance(events, list)
        assert len(events) == 1

        # Verify event structure
        event = events[0]
        assert "event_id" in event
        assert "event_type" in event
        assert "category" in event
        assert "result" in event
        assert "timestamp" in event
        assert "details" in event
        assert event["event_type"] == "harden"
        assert event["result"] == expected_result

    def test_jsonl_output_format(self):
        """Verify that --output jsonl returns one JSON object per line."""
        since = iso_now()
        time.sleep(0.05)
        run_cli("harden")
        time.sleep(0.1)

        result = run_cli(
            "events", "--event-type", "harden", "--since", since, "--output", "jsonl"
        )
        assert result.returncode == 0

        # Should be newline-delimited JSON
        lines = result.stdout.strip().split("\n")
        assert len(lines) == 1

        # Each line should be valid JSON
        event = json.loads(lines[0])
        assert isinstance(event, dict)
        assert event["event_type"] == "harden"
        assert "event_id" in event
        assert "details" in event

    def test_result_field_in_table_output(self):
        """Verify that result column shows the harden command outcome."""
        since = iso_now()
        time.sleep(0.05)
        harden_result = run_cli("harden")
        expected_result = _expected_event_result(harden_result)
        time.sleep(0.1)

        result = run_cli("events", "--event-type", "harden", "--since", since)
        assert result.returncode == 0

        # Table output should contain RESULT column with the command outcome.
        assert "RESULT" in result.stdout
        assert expected_result in result.stdout


# ---------------------------------------------------------------------------
# Tests: --summary flag
# ---------------------------------------------------------------------------


class TestEventsSummaryFlag:
    """Verify the --summary flag on the events command."""

    def test_summary_happy_path(self):
        """--summary produces a human-readable posture report after harden + verify."""
        run_cli("harden")
        run_cli("verify")
        time.sleep(0.1)

        result = run_cli("events", "--summary", "--last-hours", "1")
        assert result.returncode == 0

        out = result.stdout
        # Header
        assert "Security Posture Summary" in out
        assert "System Status:" in out
        # At least one section present
        assert "--- Hardening ---" in out
        # Footer
        assert "Total events:" in out

    def test_summary_incompatible_with_count(self):
        """--summary --count must be rejected with exit code 1."""
        result = run_cli("events", "--summary", "--count")
        assert result.returncode == 1
        assert "incompatible" in result.stderr.lower()

    def test_summary_incompatible_with_output_json(self):
        """--summary --output json must be rejected with exit code 1."""
        result = run_cli("events", "--summary", "--output", "json")
        assert result.returncode == 1
        assert "incompatible" in result.stderr.lower()


# ---------------------------------------------------------------------------
# Tests: Error event persistence
# ---------------------------------------------------------------------------


class TestFailedEventQuery:
    """Verify that persisted failed events are returned by the events CLI."""

    def test_failed_event_reads_from_sqlite(self):
        """Verify that failed events are read from SQLite with result='failed'."""
        _write_security_event_row(
            event_id="error-event-e2e",
            event_type="harden",
            category="hardening",
            result="failed",
            details={
                "request": {"config": "default"},
                "error": "loongshield not found",
                "error_type": "FileNotFoundError",
            },
            trace_id="error-trace-123",
        )

        result = run_cli("events", "--event-type", "harden", "--output", "json")
        assert result.returncode == 0, f"events query failed: {result.stderr}"

        events = json.loads(result.stdout)
        assert len(events) == 1
        event = events[0]

        assert event["event_type"] == "harden"  # NOT "harden_error"
        assert event["result"] == "failed"
        assert event["category"] == "hardening"
        assert event["details"]["error"] == "loongshield not found"
        assert event["details"]["error_type"] == "FileNotFoundError"
        assert event["trace_id"] == "error-trace-123"


# ---------------------------------------------------------------------------
# Tests: Session and run filters
# ---------------------------------------------------------------------------


def _write_session_run_filter_rows() -> None:
    rows = [
        ("target-event", "session-1", "run-1"),
        ("same-session-event", "session-1", "run-2"),
        ("same-run-event", "session-2", "run-1"),
    ]
    for event_id, session_id, run_id in rows:
        _write_security_event_row(
            event_id=event_id,
            event_type="harden",
            category="hardening",
            result="succeeded",
            trace_id=f"{event_id}-trace",
            details={},
            session_id=session_id,
            run_id=run_id,
        )


class TestSessionRunFilters:
    """Verify session and run filters use exact AND semantics."""

    def test_individual_session_and_run_filters(self):
        _write_session_run_filter_rows()

        cases = [
            (
                "--session-id",
                "session-1",
                {"target-event", "same-session-event"},
            ),
            ("--run-id", "run-1", {"target-event", "same-run-event"}),
        ]
        for option, value, expected_event_ids in cases:
            result = run_cli("events", option, value, "--output", "json")

            assert result.returncode == 0, result.stderr
            events = json.loads(result.stdout)
            assert {event["event_id"] for event in events} == expected_event_ids
            field = option.removeprefix("--").replace("-", "_")
            assert all(event[field] == value for event in events)

    def test_session_and_run_filters_match_intersection(self):
        _write_session_run_filter_rows()

        result = run_cli(
            "events",
            "--session-id",
            "session-1",
            "--run-id",
            "run-1",
            "--output",
            "json",
        )

        assert result.returncode == 0, result.stderr
        events = json.loads(result.stdout)
        assert [event["event_id"] for event in events] == ["target-event"]

        missing_result = run_cli(
            "events",
            "--session-id",
            "session-1",
            "--run-id",
            "missing-run",
            "--output",
            "json",
        )
        assert missing_result.returncode == 0, missing_result.stderr
        assert json.loads(missing_result.stdout) == []

    def test_session_and_run_coexist_with_all_existing_filters(self):
        matching_timestamp = "2026-06-09T01:00:00+00:00"
        matching_epoch = 1780966800.0
        common = {
            "event_type": "harden",
            "category": "hardening",
            "result": "succeeded",
            "trace_id": "target-trace",
            "details": {},
            "session_id": "session-1",
            "run_id": "run-1",
            "timestamp": matching_timestamp,
            "timestamp_epoch": matching_epoch,
        }
        variants = [
            ("target-event", {}),
            ("wrong-session", {"session_id": "session-2"}),
            ("wrong-run", {"run_id": "run-2"}),
            ("wrong-trace", {"trace_id": "other-trace"}),
            ("wrong-event-type", {"event_type": "verify"}),
            ("wrong-category", {"category": "asset_verify"}),
            (
                "outside-time-range",
                {
                    "timestamp": "2026-06-08T01:00:00+00:00",
                    "timestamp_epoch": 1780880400.0,
                },
            ),
        ]
        for event_id, overrides in variants:
            _write_security_event_row(
                event_id=event_id,
                **{**common, **overrides},
            )

        result = run_cli(
            "events",
            "--session-id",
            "session-1",
            "--run-id",
            "run-1",
            "--event-type",
            "harden",
            "--category",
            "hardening",
            "--trace-id",
            "target-trace",
            "--since",
            "2026-06-09T00:30:00+00:00",
            "--until",
            "2026-06-09T01:30:00+00:00",
            "--output",
            "json",
        )

        assert result.returncode == 0, result.stderr
        events = json.loads(result.stdout)
        assert [event["event_id"] for event in events] == ["target-event"]

    def test_session_and_run_coexist_with_last_hours(self):
        timestamp_epoch = time.time()
        timestamp = datetime.fromtimestamp(timestamp_epoch, tz=timezone.utc).isoformat()
        _write_security_event_row(
            event_id="recent-target",
            event_type="harden",
            category="hardening",
            result="succeeded",
            trace_id="recent-target-trace",
            details={},
            session_id="session-1",
            run_id="run-1",
            timestamp=timestamp,
            timestamp_epoch=timestamp_epoch,
        )

        result = run_cli(
            "events",
            "--session-id",
            "session-1",
            "--run-id",
            "run-1",
            "--last-hours",
            "1",
            "--output",
            "json",
        )

        assert result.returncode == 0, result.stderr
        events = json.loads(result.stdout)
        assert [event["event_id"] for event in events] == ["recent-target"]


# ---------------------------------------------------------------------------
# Tests: Events default output and format validation (TC-005, TC-006)
# ---------------------------------------------------------------------------


class TestEventsDefaultOutput:
    """Verify events command default output formats (TC-005, TC-006)."""

    def test_default_table_output(self):
        """TC-005: Default output is human-readable table format."""
        since = iso_now()
        time.sleep(0.05)
        harden_result = run_cli("harden")
        expected_result = _expected_event_result(harden_result)
        time.sleep(0.1)

        result = run_cli("events", "--event-type", "harden", "--since", since)
        assert result.returncode == 0

        # Default output is table — should NOT be parseable as JSON
        lines = result.stdout.strip().split("\n")
        # Header + 1 data row + blank line + footer
        assert len(lines) == 4
        assert lines[0].startswith("EVENT_TYPE")
        assert "harden" in lines[1]
        assert expected_result in lines[1]
        assert "1 event" in lines[3]

    def test_json_output_completeness(self):
        """TC-006: JSON output contains complete event structure.

        NOTE: Harden event structure varies depending on:
        - Whether loongshield is installed
        - Whether harden ran in scan or reinforce mode
        - The actual output from the harden command

        We verify core fields that ALWAYS exist, and make seharden summary
        statistics conditional.
        """
        since = iso_now()
        time.sleep(0.05)
        run_cli("harden")
        time.sleep(0.1)

        result = run_cli(
            "events", "--event-type", "harden", "--since", since, "--output", "json"
        )
        assert result.returncode == 0

        events = json.loads(result.stdout)
        assert isinstance(events, list)
        assert len(events) == 1

        # Verify complete event structure (core fields - always present)
        event = events[0]
        assert "event_id" in event
        assert "event_type" in event
        assert "category" in event
        assert "result" in event
        assert "timestamp" in event
        assert "trace_id" in event
        assert "pid" in event
        assert "uid" in event
        assert "session_id" in event
        assert "details" in event

        # Verify details structure
        assert "request" in event["details"]
        assert "result" in event["details"]

        # Verify result sub-object contains command execution info
        # Structure varies: may have argv (raw command) or mode/config (parsed)
        result_data = event["details"]["result"]
        assert "argv" in result_data or "mode" in result_data

        # Statistical fields are present when loongshield emits a parseable
        # seharden summary. A non-compliant scan may still exit 1 with stats.
        if "passed" in result_data:
            # If one statistical field exists, all should exist
            assert (
                "failed" in result_data
            ), "Expected 'failed' field when 'passed' exists"
            assert "total" in result_data, "Expected 'total' field when 'passed' exists"
            # Validate consistency
            assert isinstance(result_data["passed"], int)
            assert isinstance(result_data["failed"], int)
            assert isinstance(result_data["total"], int)

    def test_harden_event_with_loongshield_stats(self):
        """TC-006 (extended): When loongshield is installed, verify full stats.

        This test validates that when loongshield is available, the harden
        event contains complete parsed seharden summary statistics.
        """
        require_loongshield()

        since = iso_now()
        time.sleep(0.05)
        run_cli("harden")
        time.sleep(0.1)

        result = run_cli(
            "events", "--event-type", "harden", "--since", since, "--output", "json"
        )
        assert result.returncode == 0

        events = json.loads(result.stdout)
        assert len(events) == 1

        # With loongshield, statistical fields should be present when seharden
        # emits a parseable summary.
        result_data = events[0]["details"]["result"]
        assert "passed" in result_data, "Expected 'passed' field with loongshield"
        assert "failed" in result_data, "Expected 'failed' field with loongshield"
        assert "fixed" in result_data, "Expected 'fixed' field with loongshield"
        assert "manual" in result_data, "Expected 'manual' field with loongshield"
        assert (
            "dry_run_pending" in result_data
        ), "Expected 'dry_run_pending' field with loongshield"
        assert "total" in result_data, "Expected 'total' field with loongshield"

        # Validate data types and consistency
        assert isinstance(result_data["passed"], int)
        assert isinstance(result_data["failed"], int)
        assert isinstance(result_data["fixed"], int)
        assert isinstance(result_data["manual"], int)
        assert isinstance(result_data["dry_run_pending"], int)
        assert isinstance(result_data["total"], int)
        assert result_data["total"] > 0, "Total rules should be > 0"
        assert (
            result_data["passed"]
            + result_data["fixed"]
            + result_data["failed"]
            + result_data["manual"]
            + result_data["dry_run_pending"]
            == result_data["total"]
        )


# ---------------------------------------------------------------------------
# Tests: Events category filtering (TC-007, TC-024)
# ---------------------------------------------------------------------------


class TestEventsCategoryFiltering:
    """Verify events category filtering (TC-007, TC-024)."""

    def test_category_filter_hardening(self):
        """TC-007: --category hardening returns only hardening events."""
        since = iso_now()
        time.sleep(0.05)
        run_cli("harden")
        time.sleep(0.1)

        result = run_cli(
            "events", "--category", "hardening", "--since", since, "--output", "json"
        )
        assert result.returncode == 0

        events = json.loads(result.stdout)
        assert len(events) >= 1
        for event in events:
            assert event["category"] == "hardening"

    def test_prompt_scan_category_valid(self):
        """TC-024: prompt_scan is a valid category.

        The prompt_scan category is registered in lifecycle.py _ACTION_CATEGORY
        and should be recognized by the events command without warnings.

        This test verifies that the category validation works correctly.
        """
        result = run_cli("events", "--category", "prompt_scan")
        # Should NOT show warning about unknown category
        assert "Unknown category" not in result.stderr
        # Should succeed (may return 0 events if no prompt scans have been run)
        assert result.returncode == 0


# ---------------------------------------------------------------------------
# Tests: Events time range filtering (TC-008, TC-028, TC-029, TC-030)
# ---------------------------------------------------------------------------
class TestEventsTimeRange:
    """Verify events time range filtering (TC-008, TC-028, TC-029, TC-030)."""

    def test_last_hours_decimal_precision(self):
        """TC-008: --last-hours works with decimal values."""
        time.sleep(0.05)
        run_cli("harden")
        time.sleep(0.1)

        # Query with small time window (0.17 hours ≈ 10 minutes)
        result = run_cli(
            "events",
            "--event-type",
            "harden",
            "--last-hours",
            "0.17",
            "--output",
            "json",
        )
        assert result.returncode == 0
        events = json.loads(result.stdout)
        assert len(events) == 1

    def test_since_invalid_format_error(self):
        """TC-028: --since with invalid format should show friendly error.

        Expected: "Invalid time format. Expected ISO 8601 format"
        Actual: Python traceback ValueError from datetime.fromisoformat('now')
        """
        result = run_cli("events", "--since", "now")
        # Should return error with friendly message, not Python traceback
        assert result.returncode != 0
        assert "Invalid" in result.stderr or "Error" in result.stderr
        assert "Traceback" not in result.stderr

    def test_since_until_time_filtering(self):
        """TC-029/TC-030: Verify --since and --until filters work correctly with time boundaries.

        This test validates that:
        - Events created between t1 and t2 are returned by --since t1
        - Events created between t1 and t2 are NOT returned by --since t2
        - Events created between t1 and t2 are NOT returned by --until t1
        - Events created between t1 and t2 are returned by --until t2

        Uses isolated data directory (autouse fixture) to prevent parallel test interference.
        """
        # Step 1: Record time t1
        t1 = iso_now()
        time.sleep(0.1)  # Small delay to ensure timestamp ordering

        # Step 2: Create an event (scan-code)
        run_cli("scan-code", "--code", "print('hello')", "--language", "python")
        time.sleep(0.1)  # Allow SQLite to flush

        # Step 3: Record time t2 (after event creation)
        t2 = iso_now()

        # Step 4: events --since t1 should have results (event created after t1)
        result_since_t1 = run_cli("events", "--since", t1, "--output", "json")
        assert result_since_t1.returncode == 0
        events_since_t1 = json.loads(result_since_t1.stdout)
        assert (
            len(events_since_t1) == 1
        ), f"Expected 1 event with --since t1, got {len(events_since_t1)}"
        assert events_since_t1[0]["event_type"] == "code_scan"

        # Step 5: events --since t2 should have NO results (event created before t2)
        result_since_t2 = run_cli("events", "--since", t2, "--output", "json")
        assert result_since_t2.returncode == 0
        events_since_t2 = json.loads(result_since_t2.stdout)
        assert (
            len(events_since_t2) == 0
        ), f"Expected 0 events with --since t2, got {len(events_since_t2)}"

        # Step 6: events --until t1 should have NO results (event created after t1)
        result_until_t1 = run_cli("events", "--until", t1, "--output", "json")
        assert result_until_t1.returncode == 0
        events_until_t1 = json.loads(result_until_t1.stdout)
        assert (
            len(events_until_t1) == 0
        ), f"Expected 0 events with --until t1, got {len(events_until_t1)}"

        # Step 7: events --until t2 should have results (event created before t2)
        result_until_t2 = run_cli("events", "--until", t2, "--output", "json")
        assert result_until_t2.returncode == 0
        events_until_t2 = json.loads(result_until_t2.stdout)
        assert (
            len(events_until_t2) == 1
        ), f"Expected 1 event with --until t2, got {len(events_until_t2)}"
        assert events_until_t2[0]["event_type"] == "code_scan"


# ---------------------------------------------------------------------------
# Tests: Events pagination (TC-032, TC-033, TC-034)
# ---------------------------------------------------------------------------


class TestEventsPagination:
    """Verify events pagination parameters (TC-032, TC-033, TC-034)."""

    def _create_multiple_events(self, count: int = 10):
        """Helper to create multiple events for pagination testing."""
        for _ in range(count):
            run_cli("harden")
            time.sleep(0.05)

    def test_limit_parameter(self):
        """TC-032: --limit restricts number of returned events."""
        self._create_multiple_events(10)
        time.sleep(0.1)

        result = run_cli("events", "--limit", "3", "--output", "json")
        assert result.returncode == 0
        events = json.loads(result.stdout)
        assert len(events) == 3

    def test_limit_invalid_value(self):
        """TC-032: --limit with invalid value returns error."""
        result = run_cli("events", "--limit", "aaa")
        assert result.returncode != 0
        assert "Invalid value" in result.stderr or "Error" in result.stderr

    def test_offset_pagination(self):
        """TC-033: --offset skips N results."""
        self._create_multiple_events(13)
        time.sleep(0.1)

        # Get all events count
        result_all = run_cli("events", "--output", "json")
        all_events = json.loads(result_all.stdout)
        total_count = len(all_events)
        assert total_count >= 13

        # Query with offset 5
        result_offset = run_cli("events", "--offset", "5", "--output", "json")
        assert result_offset.returncode == 0
        offset_events = json.loads(result_offset.stdout)
        assert len(offset_events) == total_count - 5

    def test_offset_with_limit(self):
        """TC-033: --offset works with --limit for proper pagination."""
        self._create_multiple_events(10)
        time.sleep(0.1)

        result = run_cli("events", "--offset", "5", "--limit", "3", "--output", "json")
        assert result.returncode == 0
        events = json.loads(result.stdout)
        assert len(events) == 3

    def test_offset_invalid_value(self):
        """TC-033: --offset with invalid value returns error."""
        result = run_cli("events", "--offset", "aaa")
        assert result.returncode != 0
        assert "Invalid value" in result.stderr or "Error" in result.stderr

    def test_count_with_offset(self):
        """TC-034: --count should respect --offset parameter.

        Expected: Count of events after applying offset
        """
        self._create_multiple_events(13)
        time.sleep(0.1)

        # Get total count
        result_total = run_cli("events", "--count")
        total_count = json.loads(result_total.stdout)

        # Get count with offset
        result_offset = run_cli("events", "--offset", "10", "--count")
        offset_count = json.loads(result_offset.stdout)

        # Should be different (offset count < total count)
        assert offset_count < total_count

    def test_count_by_with_offset(self):
        """TC-034 (extended): --count-by should respect --offset parameter.

        Expected: Count-by statistics after applying offset
        """
        self._create_multiple_events(13)
        time.sleep(0.1)

        # Get full count-by
        result_full = run_cli("events", "--count-by", "category", "--output", "json")
        assert result_full.returncode == 0
        full_counts = json.loads(result_full.stdout)

        total_events = sum(full_counts.values())
        assert total_events >= 13

        # Get count-by with offset 10
        result_offset = run_cli(
            "events", "--offset", "10", "--count-by", "category", "--output", "json"
        )
        assert result_offset.returncode == 0
        offset_counts = json.loads(result_offset.stdout)

        offset_total = sum(offset_counts.values())

        # After offset 10, should have fewer events
        assert offset_total < total_events
        assert offset_total == total_events - 10


# ---------------------------------------------------------------------------
# Tests: Events output formats (TC-035, TC-036, TC-037)
# ---------------------------------------------------------------------------


class TestEventsOutputFormats:
    """Verify events output format compatibility (TC-035, TC-036, TC-037)."""

    def test_json_output_format(self):
        """TC-035: --output json returns valid JSON array."""
        since = iso_now()
        time.sleep(0.05)
        run_cli("harden")
        time.sleep(0.1)

        result = run_cli(
            "events", "--event-type", "harden", "--since", since, "--output", "json"
        )
        assert result.returncode == 0
        events = json.loads(result.stdout)
        assert isinstance(events, list)

    def test_jsonl_output_format(self):
        """TC-035: --output jsonl returns one JSON object per line."""
        since = iso_now()
        time.sleep(0.05)
        run_cli("harden")
        time.sleep(0.1)

        result = run_cli(
            "events", "--event-type", "harden", "--since", since, "--output", "jsonl"
        )
        assert result.returncode == 0

        lines = result.stdout.strip().split("\n")
        assert len(lines) == 1

        # Each line should be valid JSON
        event = json.loads(lines[0])
        assert isinstance(event, dict)
        assert event["event_type"] == "harden"

    def test_table_output_format(self):
        """TC-035: --output table returns formatted table."""
        since = iso_now()
        time.sleep(0.05)
        run_cli("harden")
        time.sleep(0.1)

        result = run_cli(
            "events", "--event-type", "harden", "--since", since, "--output", "table"
        )
        assert result.returncode == 0
        assert "EVENT_TYPE" in result.stdout
        assert "harden" in result.stdout

    def test_summary_table_incompatibility(self):
        """TC-036: --summary with --output table should show error.

        Expected: Error "--summary is incompatible with --output"
        Actual: Executes normally (only --output json is checked in cli.py)
        Root cause: Validation only checks `output != "table"`, should check
                    all non-default output formats
        """
        run_cli("harden")
        time.sleep(0.1)

        result = run_cli("events", "--summary", "--output", "table")
        # Should return error
        assert result.returncode != 0
        assert "incompatible" in result.stderr.lower()

    def test_summary_json_incompatibility(self):
        """TC-037: --summary with --output json shows error."""
        result = run_cli("events", "--summary", "--output", "json")
        assert result.returncode != 0
        assert "incompatible" in result.stderr.lower()


# ---------------------------------------------------------------------------
# Tests: Events summary calculation (TC-009, TC-010, TC-013, TC-015, TC-019, TC-023)
# ---------------------------------------------------------------------------


class TestEventsSummaryCalculation:
    """Verify events summary calculations (TC-009, TC-010, TC-013, TC-015, TC-019, TC-023)."""

    def test_compliance_after_rescan(self):
        """TC-010: Compliance calculation after re-scan shows correct result."""
        # This test requires loongshield to produce scan result data
        require_loongshield()

        # Run scan multiple times
        run_cli("harden")
        time.sleep(0.1)
        run_cli("harden")
        time.sleep(0.1)

        result = run_cli("events", "--summary", "--last-hours", "1")
        assert result.returncode == 0

        # Should show scan results with compliance data
        assert "Compliance:" in result.stdout
        assert "Scans performed:" in result.stdout

    def test_summary_with_verify_failures(self):
        """TC-013: Summary shows verify failures correctly."""
        run_cli("harden")
        time.sleep(0.1)
        run_cli("verify")
        time.sleep(0.1)

        result = run_cli("events", "--summary", "--last-hours", "1")
        assert result.returncode == 0

        # Should show hardening section
        assert "--- Hardening ---" in result.stdout
        # Should show verification section (may show failures)
        assert "Total events:" in result.stdout

    def test_sandbox_summary_interventions(self):
        """TC-019: Sandbox summary shows total interventions count."""
        # Create sandbox events (log-sandbox is hidden command)
        run_cli("log-sandbox")
        time.sleep(0.05)
        run_cli("log-sandbox", "--command", "rm -rf a.txt")
        time.sleep(0.1)

        result = run_cli("events", "--category", "sandbox", "--summary")
        assert result.returncode == 0

        # Should show intervention count
        assert "Total interventions:" in result.stdout
        assert "2" in result.stdout

    def test_code_scan_verdict_statistics(self):
        """TC-023: Code scan summary shows verdict statistics."""
        # Run scans with different verdicts
        run_cli("scan-code", "--code", "rm -f a.txt", "--language", "bash")  # pass
        time.sleep(0.05)
        run_cli("scan-code", "--code", "rm -rf a.txt", "--language", "bash")  # warn
        time.sleep(0.1)

        result = run_cli("events", "--category", "code_scan", "--summary")
        assert result.returncode == 0

        # Should show verdict statistics
        assert "--- Code Scanning ---" in result.stdout
        assert "Verdict:" in result.stdout or "Scans performed:" in result.stdout


# ---------------------------------------------------------------------------
# Tests: Events parameter validation (TC-025, TC-026, TC-027)
# ---------------------------------------------------------------------------


class TestEventTypeValidation:
    """Verify events parameter validation (TC-025, TC-026, TC-027)."""

    def test_event_type_no_argument(self):
        """TC-025: --event-type without argument shows error."""
        result = run_cli("events", "--event-type")
        assert result.returncode != 0
        assert "requires an argument" in result.stderr

    def test_event_type_invalid_value(self):
        """TC-025: --event-type with invalid value shows warning."""
        result = run_cli(
            "events", "--event-type", "unknown_type_xyz", "--output", "json"
        )
        # Should succeed but show warning
        assert result.returncode == 0
        assert "Warning:" in result.stderr
        assert "Unknown event_type" in result.stderr

        # Should return empty results
        events = json.loads(result.stdout)
        assert events == []

    def test_event_type_valid_value(self):
        """TC-025: --event-type with valid value filters correctly."""
        since = iso_now()
        time.sleep(0.05)
        run_cli("harden")
        time.sleep(0.1)

        result = run_cli(
            "events", "--event-type", "harden", "--since", since, "--output", "json"
        )
        assert result.returncode == 0
        events = json.loads(result.stdout)
        assert len(events) >= 1
        assert events[0]["event_type"] == "harden"

    def test_category_no_argument(self):
        """TC-026: --category without argument shows error."""
        result = run_cli("events", "--category")
        assert result.returncode != 0
        assert "requires an argument" in result.stderr

    def test_category_invalid_value(self):
        """TC-026: --category with invalid value shows warning."""
        result = run_cli(
            "events", "--category", "unknown_category_xyz", "--output", "json"
        )
        # Should succeed but show warning
        assert result.returncode == 0
        assert "Warning:" in result.stderr
        assert "Unknown category" in result.stderr

        # Should return empty results
        events = json.loads(result.stdout)
        assert events == []

    def test_category_valid_value(self):
        """TC-026: --category with valid value filters correctly."""
        since = iso_now()
        time.sleep(0.05)
        run_cli("verify")
        time.sleep(0.1)

        result = run_cli(
            "events", "--category", "asset_verify", "--since", since, "--output", "json"
        )
        assert result.returncode == 0
        events = json.loads(result.stdout)
        assert len(events) >= 1
        assert events[0]["category"] == "asset_verify"

    def test_trace_id_valid_lookup(self):
        """TC-027: --trace-id with valid ID returns matching event."""
        since = iso_now()
        time.sleep(0.05)
        run_cli("harden")
        time.sleep(0.1)

        # Get trace_id from JSON output
        result = run_cli(
            "events", "--event-type", "harden", "--since", since, "--output", "json"
        )
        events = json.loads(result.stdout)
        assert len(events) >= 1
        trace_id = events[0]["trace_id"]

        # Query by trace_id
        result = run_cli("events", "--trace-id", trace_id, "--output", "json")
        assert result.returncode == 0
        events = json.loads(result.stdout)
        assert len(events) == 1
        assert events[0]["trace_id"] == trace_id

    def test_trace_id_nonexistent(self):
        """TC-027: --trace-id with non-existent ID returns empty."""
        result = run_cli(
            "events",
            "--trace-id",
            "00000000-0000-0000-0000-000000000000",
            "--output",
            "json",
        )
        assert result.returncode == 0
        events = json.loads(result.stdout)
        assert events == []

    def test_trace_id_invalid_format(self):
        """TC-027: --trace-id with invalid format returns empty (no error)."""
        result = run_cli("events", "--trace-id", "wrongid", "--output", "json")
        assert result.returncode == 0
        events = json.loads(result.stdout)
        assert events == []


# ---------------------------------------------------------------------------
# Tests: CLI help and version (TC-001, TC-038)
# ---------------------------------------------------------------------------


class TestEventsHelpAndVersion:
    """Verify CLI help and version options (TC-001, TC-038)."""

    def test_main_help_format(self):
        """TC-001: Main help message shows all commands with aligned descriptions.

        Optimization items (not failures):
        - scan-code description missing period at end
        - scan-prompt description missing period at end
        """
        result = run_cli("--help")
        assert result.returncode == 0

        # Verify all expected commands are listed
        assert "harden" in result.stdout
        assert "verify" in result.stdout
        assert "scan-code" in result.stdout
        assert "events" in result.stdout
        assert "skill-ledger" in result.stdout
        assert "scan-prompt" in result.stdout

        # Verify help structure
        assert "Commands" in result.stdout

    def test_version_option(self):
        """TC-038: --version option should show version number.

        Expected: "agent-sec-cli <semver>" (e.g. "agent-sec-cli 0.4.0")
        """
        import re

        result = run_cli("--version")
        assert result.returncode == 0
        assert "agent-sec-cli" in result.stdout
        # Verify a semver-like version number follows "agent-sec-cli"
        assert re.search(
            r"agent-sec-cli\s+\d+\.\d+\.\d+", result.stdout
        ), f"Expected semver after 'agent-sec-cli', got: {result.stdout!r}"
