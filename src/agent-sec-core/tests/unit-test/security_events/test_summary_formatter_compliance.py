"""Compliance math in the security posture summary must respect chronology.

``agent-sec-cli events --summary`` adds the ``fixed`` count of reinforce
operations to the ``passed`` count of the latest scan. Reinforcements that
already happened *before* a newer rescan are already reflected in that scan's
``passed`` count, so adding them again inflates compliance (and can exceed
100%).
"""

from datetime import datetime, timedelta, timezone
from typing import Any

from agent_sec_cli.security_events.schema import SecurityEvent
from agent_sec_cli.security_events.summary_formatter import format_summary


def _hardening_event(
    minutes_ago: int,
    args: list[str],
    result: dict[str, Any],
) -> SecurityEvent:
    timestamp = datetime.now(timezone.utc) - timedelta(minutes=minutes_ago)
    return SecurityEvent(
        event_type="harden",
        category="hardening",
        result="succeeded",
        details={"request": {"args": args}, "result": result},
        timestamp=timestamp.isoformat(),
    )


def _scan(
    passed: int,
    failed: int,
    failures: list[dict[str, str]],
    *,
    total: int = 100,
) -> dict[str, Any]:
    return {
        "mode": "scan",
        "passed": passed,
        "failed": failed,
        "total": total,
        "failures": failures,
        "fixed": 0,
        "manual": 0,
        "dry_run_pending": 0,
    }


def _reinforce(fixed: int, *, total: int = 100) -> dict[str, Any]:
    return {
        "mode": "reinforce",
        "passed": total - fixed,
        "failed": 0,
        "total": total,
        "failures": [],
        "fixed": fixed,
        "manual": 0,
        "dry_run_pending": 0,
        "fixed_items": ["R1"],
    }


def test_rescan_after_reinforce_does_not_double_count_fixed_rules():
    """scan(50/100) -> reinforce(10 fixed) -> rescan(60/100) is 60/100, not 70/100."""
    events = [
        _hardening_event(
            15, ["--scan"], _scan(50, 50, [{"rule_id": "R1", "status": "FAIL"}])
        ),
        _hardening_event(10, ["--reinforce"], _reinforce(10)),
        _hardening_event(
            5, ["--scan"], _scan(60, 40, [{"rule_id": "R1", "status": "FAIL"}])
        ),
    ]

    output = format_summary(events, "last 24 hours")

    assert "60/100 rules passed" in output, output
    assert "70/100" not in output, output
    assert "+ 10 fixed" not in output, output


def test_reinforce_without_rescan_still_adds_fixed_rules():
    """scan(15/23) -> reinforce(8 fixed) without rescan stays 23/23."""
    events = [
        _hardening_event(
            10,
            ["--scan"],
            _scan(15, 8, [{"rule_id": "R1", "status": "FAIL"}], total=23),
        ),
        _hardening_event(5, ["--reinforce"], _reinforce(8, total=23)),
    ]

    output = format_summary(events, "last 24 hours")

    assert "23/23 rules passed" in output, output
    assert "8 fixed" in output, output
