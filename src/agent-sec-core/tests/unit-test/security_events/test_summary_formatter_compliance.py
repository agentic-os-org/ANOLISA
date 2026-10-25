"""The latest usable scan bounds hardening fix attribution."""

from datetime import datetime, timedelta, timezone

import pytest

from agent_sec_cli.security_events.schema import SecurityEvent
from agent_sec_cli.security_events.summary_formatter import format_summary


def _event(mode, minute, *, passed=60, fixed=0, total=100, result="succeeded"):
    timestamp = datetime(2026, 1, 1, tzinfo=timezone.utc) + timedelta(minutes=minute)
    return SecurityEvent(
        event_type="harden",
        category="hardening",
        result=result,
        timestamp=timestamp.isoformat(),
        details={
            "request": {"args": [f"--{mode}"]},
            "result": {
                "mode": mode,
                "passed": passed,
                "total": total,
                "fixed": fixed,
                "failures": [],
            },
        },
    )


@pytest.mark.parametrize("reverse", [False, True])
def test_rescan_does_not_recount_prior_repairs(reverse):
    events = [
        _event("scan", 0),
        _event("reinforce", 1, fixed=10),
        _event("scan", 2),
    ]
    output = format_summary(list(reversed(events)) if reverse else events, "fixture")

    assert "Compliance: 60/100 rules passed (60.0%)" in output
    assert "Reinforcements:   1 (succeeded: 1, failed: 0)" in output
    assert "Total events: 3" in output


def test_only_repairs_after_the_latest_scan_contribute():
    events = [
        _event("reinforce", 0, fixed=35),
        _event("scan", 1, passed=50),
        _event("reinforce", 2, fixed=20),
        _event("scan", 3, passed=70),
        _event("reinforce", 4, fixed=3),
        _event("reinforce", 5, fixed=7),
    ]
    output = format_summary(events, "fixture")

    assert "Compliance: 80/100 rules passed (70 passed + 10 fixed, 80.0%)" in output
    assert "Reinforcements:   4 (succeeded: 4, failed: 0)" in output


@pytest.mark.parametrize("reverse", [False, True])
def test_equal_timestamp_does_not_claim_a_repair_happened_after_scan(reverse):
    events = [_event("scan", 1), _event("reinforce", 1, fixed=10)]
    output = format_summary(list(reversed(events)) if reverse else events, "fixture")

    assert "Compliance: 60/100 rules passed (60.0%)" in output


def test_noncompliant_scan_with_statistics_is_a_new_baseline():
    events = [
        _event("scan", 0, passed=40),
        _event("reinforce", 1, fixed=20),
        _event("scan", 2, passed=60, result="failed"),
        _event("reinforce", 3, fixed=5),
    ]
    output = format_summary(events, "fixture")

    assert "Compliance: 65/100 rules passed (60 passed + 5 fixed, 65.0%)" in output
    assert "Scans performed:  2 (succeeded: 1, failed: 1)" in output


def test_failed_scan_without_statistics_does_not_replace_the_baseline():
    events = [
        _event("scan", 0),
        _event("reinforce", 1, fixed=10),
        _event("scan", 2, total=0, result="failed"),
    ]
    output = format_summary(events, "fixture")

    assert "Compliance: 70/100 rules passed (60 passed + 10 fixed, 70.0%)" in output


def test_post_scan_repairs_and_dry_run_keep_their_existing_meaning():
    events = [
        _event("scan", 0),
        _event("reinforce", 1, fixed=10),
        _event("dry-run", 2, fixed=30),
        _event("reinforce", 3, fixed=5, result="failed"),
        _event("reinforce", 4, fixed=99, total=0, result="failed"),
    ]
    output = format_summary(events, "fixture")

    assert "Compliance: 75/100 rules passed (60 passed + 15 fixed, 75.0%)" in output
    assert "Reinforcements:   3 (succeeded: 1, failed: 2)" in output


def test_timestamp_offsets_are_normalized_before_attributing_repairs():
    scan = _event("scan", 1)
    repair = _event("reinforce", 0, fixed=10).model_copy(
        update={"timestamp": "2026-01-01T08:00:00+08:00"}
    )
    # Revalidate through the public envelope, as JSONL/SQLite readers do.
    repair = SecurityEvent.model_validate(repair.model_dump())
    output = format_summary([scan, repair], "fixture")

    assert "Compliance: 60/100 rules passed (60.0%)" in output


def test_reinforcement_only_history_does_not_invent_a_scan_baseline():
    output = format_summary([_event("reinforce", 0, fixed=10)], "fixture")

    assert "Reinforcements:   1 (succeeded: 1, failed: 0)" in output
    assert "Compliance:" not in output
