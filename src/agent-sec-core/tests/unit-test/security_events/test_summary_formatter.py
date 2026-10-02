"""Unit tests for security_events.summary_formatter."""

from datetime import datetime, timedelta, timezone
from typing import Any

from agent_sec_cli.security_events.schema import SecurityEvent
from agent_sec_cli.security_events.summary_formatter import format_summary

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_event(
    event_type: str = "test",
    category: str = "test",
    result: str = "succeeded",
    details: dict[str, Any] | None = None,
    timestamp: str | None = None,
) -> SecurityEvent:
    if timestamp is None:
        timestamp = datetime.now(timezone.utc).isoformat()
    return SecurityEvent(
        event_type=event_type,
        category=category,
        result=result,
        details=details or {},
        timestamp=timestamp,
    )


def _ts_minutes_ago(minutes: int) -> str:
    dt = datetime.now(timezone.utc) - timedelta(minutes=minutes)
    return dt.isoformat()


# ---------------------------------------------------------------------------
# Test: empty events
# ---------------------------------------------------------------------------


class TestEmptyEvents:
    def test_empty_list_returns_guidance_message(self):
        output = format_summary([], "last 24 hours")
        assert "No security events recorded" in output


# ---------------------------------------------------------------------------
# Test: hardening summary
# ---------------------------------------------------------------------------


class TestHardeningSummary:
    def test_compliance_includes_fixed_count(self):
        """Compliance calculation should include fixed items: (passed + fixed) / total.

        After reinforce without rescan, the compliance should reflect both
        passed items from scan and fixed items from reinforce.

        Example: 15 passed from scan + 8 fixed from reinforce = 23/23 (100%)
        """
        events = [
            # Initial scan with failures
            _make_event(
                event_type="harden",
                category="hardening",
                result="succeeded",
                details={
                    "request": {"args": ["--scan", "--config", "agentos_baseline"]},
                    "result": {
                        "mode": "scan",
                        "config": "agentos_baseline",
                        "passed": 15,
                        "failed": 8,
                        "total": 23,
                        "failures": [
                            {
                                "rule_id": "SEC-001",
                                "status": "FAIL",
                                "message": "SSH config",
                            },
                            {
                                "rule_id": "SEC-002",
                                "status": "FAIL",
                                "message": "Firewall",
                            },
                        ],
                        "fixed": 0,
                        "manual": 0,
                        "dry_run_pending": 0,
                        "fixed_items": [],
                    },
                },
                timestamp=_ts_minutes_ago(10),
            ),
            # Reinforce operation that fixes the failures
            _make_event(
                event_type="harden",
                category="hardening",
                result="succeeded",
                details={
                    "request": {
                        "args": ["--reinforce", "--config", "agentos_baseline"]
                    },
                    "result": {
                        "mode": "reinforce",
                        "config": "agentos_baseline",
                        "passed": 15,
                        "failed": 0,
                        "total": 23,
                        "failures": [],
                        "fixed": 8,
                        "manual": 0,
                        "dry_run_pending": 0,
                        "fixed_items": ["SEC-001", "SEC-002"],
                    },
                },
                timestamp=_ts_minutes_ago(5),
            ),
        ]
        output = format_summary(events, "last 24 hours")

        # Should show 100% compliance: (15 passed + 8 fixed) / 23 total
        assert "Compliance:" in output
        # The compliance should reflect the fixed count
        assert "100.0%" in output or "23/23" in output

    def test_scan_count_and_compliance(self):
        events = [
            _make_event(
                event_type="harden",
                category="hardening",
                result="succeeded",
                details={
                    "request": {"args": ["--scan", "--config", "agentos_baseline"]},
                    "result": {
                        "mode": "scan",
                        "config": "agentos_baseline",
                        "passed": 45,
                        "failed": 3,
                        "total": 48,
                        "failures": [
                            {
                                "rule_id": "SEC-001",
                                "status": "FAIL",
                                "message": "SSH root login not disabled",
                            },
                            {
                                "rule_id": "SEC-003",
                                "status": "FAIL",
                                "message": "Firewall rules not updated",
                            },
                            {
                                "rule_id": "SEC-005",
                                "status": "FAIL",
                                "message": "Audit log disabled",
                            },
                        ],
                        "fixed": 0,
                        "manual": 0,
                        "dry_run_pending": 0,
                        "fixed_items": [],
                    },
                },
                timestamp=_ts_minutes_ago(10),
            ),
            _make_event(
                event_type="harden",
                category="hardening",
                result="succeeded",
                details={
                    "request": {"args": ["--scan", "--config", "agentos_baseline"]},
                    "result": {
                        "mode": "scan",
                        "config": "agentos_baseline",
                        "passed": 48,
                        "failed": 0,
                        "total": 48,
                        "failures": [],
                        "fixed": 0,
                        "manual": 0,
                        "dry_run_pending": 0,
                        "fixed_items": [],
                    },
                },
                timestamp=_ts_minutes_ago(60),
            ),
        ]
        output = format_summary(events, "last 24 hours")

        assert "--- Hardening ---" in output
        assert "Scans performed:  2 (succeeded: 2, failed: 0)" in output
        assert "45/48 rules passed (93.8%)" in output
        assert "agent-sec-cli harden --scan" in output

    def test_reinforcement_count(self):
        events = [
            _make_event(
                event_type="harden",
                category="hardening",
                result="succeeded",
                details={
                    "request": {
                        "args": ["--reinforce", "--config", "agentos_baseline"]
                    },
                    "result": {
                        "mode": "reinforce",
                        "config": "agentos_baseline",
                        "passed": 48,
                        "failed": 0,
                        "total": 48,
                        "failures": [],
                        "fixed": 2,
                        "manual": 0,
                        "dry_run_pending": 0,
                        "fixed_items": [],
                    },
                },
                timestamp=_ts_minutes_ago(5),
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Reinforcements:   1 (succeeded: 1, failed: 0)" in output

    def test_failed_scan(self):
        events = [
            _make_event(
                event_type="harden",
                category="hardening",
                result="failed",
                details={
                    "request": {"args": ["--scan", "--config", "agentos_baseline"]},
                    "error": "loongshield not found",
                    "error_type": "FileNotFoundError",
                },
                timestamp=_ts_minutes_ago(5),
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Scans performed:  1 (succeeded: 0, failed: 1)" in output
        assert "Latest scan failed: loongshield not found" in output
        # Latest harden failed -> needs_attention (not critical)
        assert "Needs attention" in output

    def test_failed_scan_with_stats_still_shows_compliance(self):
        events = [
            _make_event(
                event_type="harden",
                category="hardening",
                result="failed",
                details={
                    "request": {
                        "args": ["--scan", "--config", "agentos_baseline", "--verbose"]
                    },
                    "result": {
                        "mode": "scan",
                        "config": "agentos_baseline",
                        "passed": 20,
                        "failed": 3,
                        "total": 23,
                        "failures": [
                            {
                                "rule_id": "SEC-001",
                                "status": "FAIL",
                                "message": "issue",
                            }
                        ],
                        "fixed": 0,
                        "manual": 0,
                        "dry_run_pending": 0,
                        "fixed_items": [],
                    },
                },
                timestamp=_ts_minutes_ago(5),
            )
        ]

        output = format_summary(events, "last 24 hours")

        assert "Scans performed:  1 (succeeded: 0, failed: 1)" in output
        assert "Latest scan result:" in output
        assert "Compliance: 20/23 rules passed (87.0%)" in output
        assert "Latest scan failed" not in output
        assert "agent-sec-cli harden --reinforce" in output

    def test_failed_reinforce_with_stats_contributes_fixed_count(self):
        events = [
            _make_event(
                event_type="harden",
                category="hardening",
                result="failed",
                details={
                    "request": {
                        "args": ["--scan", "--config", "agentos_baseline", "--verbose"]
                    },
                    "result": {
                        "mode": "scan",
                        "config": "agentos_baseline",
                        "passed": 20,
                        "failed": 3,
                        "total": 23,
                        "failures": [
                            {
                                "rule_id": "SEC-001",
                                "status": "FAIL",
                                "message": "issue",
                            }
                        ],
                        "fixed": 0,
                        "manual": 0,
                        "dry_run_pending": 0,
                        "fixed_items": [],
                    },
                },
                timestamp=_ts_minutes_ago(10),
            ),
            _make_event(
                event_type="harden",
                category="hardening",
                result="failed",
                details={
                    "request": {
                        "args": [
                            "--reinforce",
                            "--config",
                            "agentos_baseline",
                            "--verbose",
                        ]
                    },
                    "result": {
                        "mode": "reinforce",
                        "config": "agentos_baseline",
                        "passed": 20,
                        "failed": 1,
                        "total": 23,
                        "failures": [
                            {
                                "rule_id": "SEC-003",
                                "status": "FAIL",
                                "message": "still failing",
                            }
                        ],
                        "fixed": 2,
                        "manual": 0,
                        "dry_run_pending": 0,
                        "fixed_items": [
                            {
                                "rule_id": "SEC-001",
                                "status": "FIXED",
                                "message": "fixed",
                            },
                            {
                                "rule_id": "SEC-002",
                                "status": "FIXED",
                                "message": "fixed",
                            },
                        ],
                    },
                },
                timestamp=_ts_minutes_ago(5),
            ),
        ]

        output = format_summary(events, "last 24 hours")

        assert "Reinforcements:   1 (succeeded: 0, failed: 1)" in output
        assert "Compliance: 22/23 rules passed (20 passed + 2 fixed, 95.7%)" in output


# ---------------------------------------------------------------------------
# Test: asset verify summary
# ---------------------------------------------------------------------------


class TestAssetVerifySummary:
    def test_successful_verifications(self):
        events = [
            _make_event(
                event_type="verify",
                category="asset_verify",
                result="succeeded",
                details={
                    "request": {"skill": None},
                    "result": {"passed": 5, "failed": 0},
                },
                timestamp=_ts_minutes_ago(14),
            ),
            _make_event(
                event_type="verify",
                category="asset_verify",
                result="succeeded",
                details={
                    "request": {"skill": "/opt/skills/my-skill"},
                    "result": {"passed": 3, "failed": 1},
                },
                timestamp=_ts_minutes_ago(120),
            ),
        ]
        output = format_summary(events, "last 24 hours")

        assert "--- Asset Verification ---" in output
        assert (
            "Verifications performed: 2 (verified: 1, skipped: 0, failed: 1)" in output
        )
        assert "5 checked, 5 passed, 0 failed" in output
        assert "ALL CLEAR" in output

    def test_failed_verification(self):
        events = [
            _make_event(
                event_type="verify",
                category="asset_verify",
                result="succeeded",
                details={
                    "request": {"skill": None},
                    "result": {"passed": 3, "failed": 2},
                },
                timestamp=_ts_minutes_ago(5),
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "FAILURES DETECTED" in output
        assert "5 checked, 3 passed, 2 failed" in output

    def test_single_skill_verify_after_full_verify(self):
        """After full verify + single skill verify, summary shows latest result.

        When a single skill is verified after a full verify, the summary
        currently shows only the latest single-skill result.

        This test documents the current behavior: the summary formatter
        reads only the latest verify event and doesn't aggregate across
        multiple verify invocations.
        """
        events = [
            # Full verify of all skills
            _make_event(
                event_type="verify",
                category="asset_verify",
                result="succeeded",
                details={
                    "request": {"skill": None},
                    "result": {"passed": 0, "failed": 5},
                },
                timestamp=_ts_minutes_ago(10),
            ),
            # Single skill verify
            _make_event(
                event_type="verify",
                category="asset_verify",
                result="succeeded",
                details={
                    "request": {"skill": "regex-mastery"},
                    "result": {"passed": 1, "failed": 0},
                },
                timestamp=_ts_minutes_ago(5),
            ),
        ]
        output = format_summary(events, "last 24 hours")

        # Should show verification section
        # System status should be "Needs attention" because full verify had 5 failures
        # (single-skill verify should not affect posture)
        assert "Needs attention" in output
        assert "--- Asset Verification ---" in output
        assert (
            "Verifications performed: 2 (verified: 1, skipped: 0, failed: 1)" in output
        )
        # Latest result shows single skill result (1 passed, 0 failed)
        assert "Latest result:" in output
        assert "1 checked, 1 passed, 0 failed" in output

    def test_no_candidates_is_reported_as_not_assessed(self):
        events = [
            _make_event(
                event_type="verify",
                category="asset_verify",
                result="succeeded",
                details={
                    "request": {"skill": None},
                    "result": {
                        "outcome": "no_candidates",
                        "checked": 0,
                        "passed": 0,
                        "failed": 0,
                    },
                },
                timestamp=_ts_minutes_ago(5),
            ),
        ]

        output = format_summary(events, "last 24 hours")

        assert (
            "Verifications performed: 1 (verified: 0, skipped: 1, failed: 0)" in output
        )
        assert "0 checked, 0 passed, 0 failed" in output
        assert "NOT ASSESSED (no candidate skills)" in output
        assert "ALL CLEAR" not in output
        assert "System Status: Good" in output

    def test_legacy_zero_counts_are_reported_as_not_assessed(self):
        events = [
            _make_event(
                event_type="verify",
                category="asset_verify",
                result="succeeded",
                details={
                    "request": {"skill": None},
                    "result": {"passed": 0, "failed": 0},
                },
                timestamp=_ts_minutes_ago(5),
            ),
        ]

        output = format_summary(events, "last 24 hours")

        assert (
            "Verifications performed: 1 (verified: 0, skipped: 1, failed: 0)" in output
        )
        assert "NOT ASSESSED (no candidate skills)" in output
        assert "ALL CLEAR" not in output

    def test_latest_no_candidates_does_not_clear_prior_full_failure(self):
        events = [
            _make_event(
                event_type="verify",
                category="asset_verify",
                result="succeeded",
                details={
                    "request": {"skill": None},
                    "result": {
                        "outcome": "failed",
                        "checked": 1,
                        "passed": 0,
                        "failed": 1,
                    },
                },
                timestamp=_ts_minutes_ago(10),
            ),
            _make_event(
                event_type="verify",
                category="asset_verify",
                result="succeeded",
                details={
                    "request": {"skill": None},
                    "result": {
                        "outcome": "no_candidates",
                        "checked": 0,
                        "passed": 0,
                        "failed": 0,
                    },
                },
                timestamp=_ts_minutes_ago(5),
            ),
        ]

        output = format_summary(events, "last 24 hours")

        assert "NOT ASSESSED (no candidate skills)" in output
        assert "System Status: Needs attention" in output

    def test_nested_no_candidates_overrides_failed_event_envelope(self):
        events = [
            _make_event(
                event_type="verify",
                category="asset_verify",
                result="failed",
                details={
                    "request": {"skill": None},
                    "result": {
                        "outcome": "no_candidates",
                        "checked": 0,
                        "passed": 0,
                        "failed": 0,
                    },
                },
                timestamp=_ts_minutes_ago(5),
            ),
        ]

        output = format_summary(events, "last 24 hours")

        assert (
            "Verifications performed: 1 (verified: 0, skipped: 1, failed: 0)" in output
        )
        assert "NOT ASSESSED (no candidate skills)" in output
        assert "System Status: Good" in output

    def test_nested_failed_outcome_overrides_successful_event_envelope(self):
        events = [
            _make_event(
                event_type="verify",
                category="asset_verify",
                result="succeeded",
                details={
                    "request": {"skill": None},
                    "result": {
                        "outcome": "failed",
                        "checked": 1,
                        "passed": 1,
                        "failed": 0,
                    },
                },
                timestamp=_ts_minutes_ago(5),
            ),
        ]

        output = format_summary(events, "last 24 hours")

        assert (
            "Verifications performed: 1 (verified: 0, skipped: 0, failed: 1)" in output
        )
        assert "FAILURES DETECTED" in output
        assert "System Status: Needs attention" in output


# ---------------------------------------------------------------------------
# Test: code scan summary
# ---------------------------------------------------------------------------


class TestCodeScanSummary:
    def test_deny_findings(self):
        events = [
            _make_event(
                event_type="code_scan",
                category="code_scan",
                result="succeeded",
                details={
                    "request": {"code": "rm -rf /", "language": "bash"},
                    "result": {
                        "ok": False,
                        "verdict": "deny",
                        "summary": "Dangerous code detected",
                        "findings": [
                            {
                                "rule_id": "BASH-001",
                                "severity": "deny",
                                "desc_en": "Remote code execution",
                                "desc_zh": "远程代码执行",
                                "evidence": ["rm -rf /"],
                            },
                        ],
                        "language": "bash",
                        "engine_version": "0.1.0",
                        "elapsed_ms": 5,
                    },
                },
                timestamp=_ts_minutes_ago(3),
            ),
        ]
        output = format_summary(events, "last 24 hours")

        assert "--- Code Scanning ---" in output
        assert "Scans performed: 1 (succeeded: 1, failed: 0)" in output
        assert "deny: 1" in output

    def test_pass_verdict_no_deny_section(self):
        events = [
            _make_event(
                event_type="code_scan",
                category="code_scan",
                result="succeeded",
                details={
                    "request": {"code": "echo hello", "language": "bash"},
                    "result": {
                        "ok": True,
                        "verdict": "pass",
                        "summary": "No issues",
                        "findings": [],
                        "language": "bash",
                        "engine_version": "0.1.0",
                        "elapsed_ms": 2,
                    },
                },
                timestamp=_ts_minutes_ago(10),
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "pass: 1" in output
        assert "Recent deny findings" not in output

    def test_mixed_verdicts_breakdown(self):
        events = [
            _make_event(
                event_type="code_scan",
                category="code_scan",
                result="succeeded",
                details={
                    "request": {"code": "x", "language": "bash"},
                    "result": {"ok": True, "verdict": "pass", "findings": []},
                },
                timestamp=_ts_minutes_ago(1),
            ),
            _make_event(
                event_type="code_scan",
                category="code_scan",
                result="succeeded",
                details={
                    "request": {"code": "y", "language": "bash"},
                    "result": {
                        "ok": True,
                        "verdict": "warn",
                        "findings": [
                            {
                                "rule_id": "W1",
                                "severity": "warn",
                                "desc_en": "w",
                                "desc_zh": "w",
                                "evidence": [],
                            }
                        ],
                    },
                },
                timestamp=_ts_minutes_ago(2),
            ),
            _make_event(
                event_type="code_scan",
                category="code_scan",
                result="succeeded",
                details={
                    "request": {"code": "z", "language": "bash"},
                    "result": {"ok": True, "verdict": "pass", "findings": []},
                },
                timestamp=_ts_minutes_ago(3),
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "pass: 2, warn: 1" in output


# ---------------------------------------------------------------------------
# Test: sandbox summary
# ---------------------------------------------------------------------------


class TestSandboxSummary:
    def test_sandbox_interventions(self):
        events = [
            _make_event(
                event_type="sandbox_prehook",
                category="sandbox",
                result="succeeded",
                details={
                    "request": {
                        "decision": "sandbox",
                        "command": "rm -rf /tmp/data",
                        "reasons": "recursive delete",
                        "network_policy": "none",
                        "cwd": "/home/user",
                    },
                    "result": {
                        "decision": "sandbox",
                        "command": "rm -rf /tmp/data",
                        "reasons": "recursive delete",
                        "network_policy": "none",
                        "cwd": "/home/user",
                    },
                },
                timestamp=_ts_minutes_ago(2),
            ),
            _make_event(
                event_type="sandbox_prehook",
                category="sandbox",
                result="succeeded",
                details={
                    "request": {
                        "decision": "block",
                        "command": "chmod 777 /etc/shadow",
                        "reasons": "permission escalation",
                        "network_policy": "none",
                        "cwd": "/home/user",
                    },
                    "result": {
                        "decision": "block",
                        "command": "chmod 777 /etc/shadow",
                        "reasons": "permission escalation",
                        "network_policy": "none",
                        "cwd": "/home/user",
                    },
                },
                timestamp=_ts_minutes_ago(5),
            ),
        ]
        output = format_summary(events, "last 24 hours")

        assert "--- Sandbox Guard ---" in output
        assert "Total interventions: 2" in output
        # Sandbox no longer exposes raw commands for security reasons
        assert "rm -rf /tmp/data" not in output
        assert "chmod 777" not in output


# ---------------------------------------------------------------------------
# Test: prompt scan summary
# ---------------------------------------------------------------------------


def _make_prompt_scan_event(
    verdict: str,
    threat_type: str = "benign",
    summary: str = "",
    result: str = "succeeded",
    minutes_ago: int = 5,
) -> SecurityEvent:
    """Helper to build a prompt_scan SecurityEvent."""
    scan_result: dict[str, Any] = {
        "schema_version": "1.0",
        "ok": verdict == "pass",
        "verdict": verdict,
        "risk_level": {"pass": "low", "warn": "medium", "deny": "high"}.get(
            verdict, "unknown"
        ),
        "threat_type": threat_type,
        "summary": summary or f"Scan result: {verdict}",
        "findings": [],
        "layer_results": [],
        "engine_version": "0.1.0",
        "elapsed_ms": 10,
    }
    if verdict in ("warn", "deny"):
        scan_result["confidence"] = 0.85
    details: dict[str, Any] = {
        "request": {"text": "some prompt", "mode": "standard", "source": ""},
        "result": scan_result,
    }
    return _make_event(
        event_type="prompt_scan",
        category="prompt_scan",
        result=result,
        details=details,
        timestamp=_ts_minutes_ago(minutes_ago),
    )


class TestPromptScanSummary:
    def test_section_header_present(self):
        events = [_make_prompt_scan_event("pass")]
        output = format_summary(events, "last 24 hours")
        assert "--- Prompt Scan ---" in output

    def test_scan_count_succeeded(self):
        events = [
            _make_prompt_scan_event("pass", minutes_ago=1),
            _make_prompt_scan_event(
                "deny", threat_type="direct_injection", minutes_ago=2
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Scans performed: 2 (succeeded: 2, failed: 0)" in output

    def test_scan_count_with_failed_event(self):
        events = [
            _make_prompt_scan_event("pass", minutes_ago=1),
            _make_prompt_scan_event("pass", result="failed", minutes_ago=2),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Scans performed: 2 (succeeded: 1, failed: 1)" in output

    def test_verdict_breakdown_pass_only(self):
        events = [
            _make_prompt_scan_event("pass", minutes_ago=1),
            _make_prompt_scan_event("pass", minutes_ago=2),
        ]
        output = format_summary(events, "last 24 hours")
        assert "pass: 2" in output

    def test_verdict_breakdown_mixed(self):
        events = [
            _make_prompt_scan_event("pass", minutes_ago=1),
            _make_prompt_scan_event("warn", threat_type="jailbreak", minutes_ago=2),
            _make_prompt_scan_event(
                "deny", threat_type="direct_injection", minutes_ago=3
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "deny: 1" in output
        assert "pass: 1" in output
        assert "warn: 1" in output

    def test_threat_type_breakdown(self):
        events = [
            _make_prompt_scan_event("warn", threat_type="jailbreak", minutes_ago=1),
            _make_prompt_scan_event(
                "deny", threat_type="direct_injection", minutes_ago=2
            ),
            _make_prompt_scan_event(
                "deny", threat_type="direct_injection", minutes_ago=3
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "direct_injection: 2" in output
        assert "jailbreak: 1" in output

    def test_no_threat_type_section_when_all_pass(self):
        events = [
            _make_prompt_scan_event("pass", minutes_ago=1),
            _make_prompt_scan_event("pass", minutes_ago=2),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Threat types" not in output

    def test_latest_threats_shown(self):
        events = [
            _make_prompt_scan_event(
                "deny",
                threat_type="direct_injection",
                summary="[Rule] Direct Injection detected (confidence: 90.0%)",
                minutes_ago=1,
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Latest threat:" in output
        assert "DENY" in output
        assert "direct_injection" in output
        assert "Direct Injection detected" in output

    def test_at_most_3_latest_threats_shown(self):
        """Only the 3 most recent threats should appear in the latest threats list."""
        events = [
            _make_prompt_scan_event(
                "deny",
                threat_type="direct_injection",
                summary=f"threat-{i}",
                minutes_ago=i,
            )
            for i in range(1, 6)  # 5 deny events
        ]
        output = format_summary(events, "last 24 hours")
        assert "Latest threats:" in output
        # Only first 3 (newest) should appear — threats are appended newest-first
        assert "threat-1" in output
        assert "threat-2" in output
        assert "threat-3" in output
        assert "threat-4" not in output
        assert "threat-5" not in output

    def test_no_latest_threats_section_when_all_pass(self):
        events = [_make_prompt_scan_event("pass", minutes_ago=1)]
        output = format_summary(events, "last 24 hours")
        assert "Latest threat" not in output


# ---------------------------------------------------------------------------
# Test: posture computation
# ---------------------------------------------------------------------------


class TestPostureComputation:
    def test_good_status(self):
        events = [
            _make_event(
                event_type="verify",
                category="asset_verify",
                result="succeeded",
                details={"request": {}, "result": {"passed": 5, "failed": 0}},
                timestamp=_ts_minutes_ago(10),
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Good" in output
        assert "\u2713" in output

    def test_sandbox_block_does_not_affect_posture(self):
        """Sandbox events are independent — they must NOT affect posture status."""
        events = [
            _make_event(
                event_type="harden",
                category="hardening",
                result="succeeded",
                details={
                    "request": {"args": ["--scan"]},
                    "result": {
                        "mode": "scan",
                        "passed": 48,
                        "failed": 0,
                        "total": 48,
                        "failures": [],
                    },
                },
                timestamp=_ts_minutes_ago(5),
            ),
            _make_event(
                event_type="sandbox_prehook",
                category="sandbox",
                result="succeeded",
                details={
                    "request": {
                        "decision": "block",
                        "command": "rm /etc/passwd",
                        "reasons": "critical file",
                        "network_policy": "none",
                        "cwd": "/",
                    },
                    "result": {
                        "decision": "block",
                        "command": "rm /etc/passwd",
                        "reasons": "critical file",
                        "network_policy": "none",
                        "cwd": "/",
                    },
                },
                timestamp=_ts_minutes_ago(10),
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Good" in output
        assert "\u2713" in output

    def test_critical_on_harden_failure(self):
        """Latest harden failed -> needs_attention (tool error, not security violation)."""
        events = [
            _make_event(
                event_type="harden",
                category="hardening",
                result="failed",
                details={
                    "request": {"args": ["--scan"]},
                    "error": "tool not found",
                    "error_type": "FileNotFoundError",
                },
                timestamp=_ts_minutes_ago(5),
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Needs attention" in output
        assert "\u26a0" in output

    def test_code_deny_does_not_affect_posture(self):
        """Code scan is an independent category — deny must NOT affect posture."""
        events = [
            _make_event(
                event_type="code_scan",
                category="code_scan",
                result="succeeded",
                details={
                    "request": {"code": "bad", "language": "bash"},
                    "result": {
                        "ok": False,
                        "verdict": "deny",
                        "summary": "Deny",
                        "findings": [
                            {
                                "rule_id": "X",
                                "severity": "deny",
                                "desc_en": "bad",
                                "desc_zh": "bad",
                                "evidence": [],
                            }
                        ],
                        "language": "bash",
                        "engine_version": "0.1.0",
                        "elapsed_ms": 1,
                    },
                },
                timestamp=_ts_minutes_ago(1),
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Good" in output
        assert "\u2713" in output

    def test_prompt_scan_warn_does_not_affect_posture(self):
        """Prompt scan WARN verdict must NOT trigger needs_attention."""
        events = [
            _make_prompt_scan_event("warn", threat_type="jailbreak", minutes_ago=1)
        ]
        output = format_summary(events, "last 24 hours")
        assert "Good" in output
        assert "\u2713" in output

    def test_prompt_scan_deny_triggers_needs_attention(self):
        """Prompt scan DENY verdict must trigger needs_attention."""
        events = [
            _make_prompt_scan_event(
                "deny", threat_type="direct_injection", minutes_ago=1
            )
        ]
        output = format_summary(events, "last 24 hours")
        assert "Needs attention" in output
        assert "\u26a0" in output

    def test_prompt_scan_deny_failed_event_does_not_affect_posture(self):
        """A prompt_scan event that itself failed (scanner error) must NOT affect posture."""
        events = [
            _make_prompt_scan_event(
                "deny", threat_type="direct_injection", result="failed", minutes_ago=1
            )
        ]
        output = format_summary(events, "last 24 hours")
        assert "Good" in output
        assert "\u2713" in output


# ---------------------------------------------------------------------------
# Test: defensive sort (events given in wrong order)
# ---------------------------------------------------------------------------


class TestDefensiveSort:
    def test_asc_order_events_still_pick_latest(self):
        """Events passed in ascending order must still produce correct 'latest' results."""
        older = _make_event(
            event_type="harden",
            category="hardening",
            result="succeeded",
            details={
                "request": {"args": ["--scan"]},
                "result": {
                    "mode": "scan",
                    "passed": 48,
                    "failed": 0,
                    "total": 48,
                    "failures": [],
                },
            },
            timestamp=_ts_minutes_ago(60),
        )
        newer = _make_event(
            event_type="harden",
            category="hardening",
            result="succeeded",
            details={
                "request": {"args": ["--scan"]},
                "result": {
                    "mode": "scan",
                    "passed": 40,
                    "failed": 8,
                    "total": 48,
                    "failures": [{"rule_id": "SEC-001"}],
                },
            },
            timestamp=_ts_minutes_ago(5),
        )
        # Pass in ASCENDING order — older first
        output = format_summary([older, newer], "last 24 hours")
        # Latest (newer) has 40/48 compliance — not the older 48/48
        assert "40/48 rules passed" in output
        assert "Needs attention" in output


# ---------------------------------------------------------------------------
# Test: posture – verify failed path
# ---------------------------------------------------------------------------


class TestPostureVerifyFailed:
    def test_verify_event_failed_triggers_needs_attention(self):
        """Latest verify event.result=='failed' triggers needs_attention."""
        events = [
            _make_event(
                event_type="verify",
                category="asset_verify",
                result="failed",
                details={
                    "request": {"skill": None},
                    "error": "timeout",
                    "error_type": "TimeoutError",
                },
                timestamp=_ts_minutes_ago(5),
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Needs attention" in output
        assert "\u26a0" in output


# ---------------------------------------------------------------------------
# Test: suggestions edge cases
# ---------------------------------------------------------------------------


class TestSuggestionsEdgeCases:
    def test_no_suggestion_when_latest_harden_failed(self):
        """If latest harden event.result=='failed', do NOT suggest --reinforce."""
        events = [
            _make_event(
                event_type="harden",
                category="hardening",
                result="failed",
                details={"request": {"args": ["--scan"]}, "error": "not found"},
                timestamp=_ts_minutes_ago(5),
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "agent-sec-cli harden --reinforce" not in output

    def test_no_reinforce_suggestion_when_failed_harden_has_parser_failure(self):
        events = [
            _make_event(
                event_type="harden",
                category="hardening",
                result="failed",
                details={
                    "request": {"args": ["--scan"]},
                    "result": {
                        "mode": "scan",
                        "passed": 22,
                        "failed": 1,
                        "total": 23,
                        "failures": [
                            {
                                "rule_id": "",
                                "status": "UNKNOWN",
                                "message": "Summary reports non-pass rules but details failed.",
                            }
                        ],
                    },
                },
                timestamp=_ts_minutes_ago(5),
            )
        ]

        output = format_summary(events, "last 24 hours")

        assert "Compliance: 22/23 rules passed" in output
        assert "Latest scan failed" not in output
        assert "agent-sec-cli harden --reinforce" not in output

    def test_no_suggestion_when_no_hardening_events(self):
        """Only sandbox events — no suggestion at all."""
        events = [
            _make_event(
                event_type="sandbox_prehook",
                category="sandbox",
                result="succeeded",
                details={
                    "request": {"decision": "block", "command": "x"},
                    "result": {"decision": "block"},
                },
                timestamp=_ts_minutes_ago(5),
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Suggested actions" not in output


# ---------------------------------------------------------------------------
# Test: output format exact prefixes
# ---------------------------------------------------------------------------


class TestOutputFormatPrefixes:
    def test_system_status_good_prefix(self):
        events = [
            _make_event(
                event_type="verify",
                category="asset_verify",
                result="succeeded",
                details={"request": {}, "result": {"passed": 5, "failed": 0}},
                timestamp=_ts_minutes_ago(10),
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "System Status: Good" in output

    def test_system_status_needs_attention_prefix(self):
        events = [
            _make_event(
                event_type="harden",
                category="hardening",
                result="failed",
                details={"request": {"args": ["--scan"]}, "error": "err"},
                timestamp=_ts_minutes_ago(5),
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "System Status: Needs attention" in output

    def test_verdict_prefix_in_code_scan(self):
        events = [
            _make_event(
                event_type="code_scan",
                category="code_scan",
                result="succeeded",
                details={
                    "request": {"code": "ls", "language": "bash"},
                    "result": {"ok": True, "verdict": "pass", "findings": []},
                },
                timestamp=_ts_minutes_ago(5),
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Verdict: pass: 1" in output


# ---------------------------------------------------------------------------
# Test: mixed categories
# ---------------------------------------------------------------------------


class TestMixedCategories:
    def test_combined_report_structure(self):
        events = [
            _make_event(
                event_type="harden",
                category="hardening",
                result="succeeded",
                details={
                    "request": {"args": ["--scan", "--config", "agentos_baseline"]},
                    "result": {
                        "mode": "scan",
                        "config": "agentos_baseline",
                        "passed": 46,
                        "failed": 2,
                        "total": 48,
                        "failures": [
                            {"rule_id": "SEC-001", "status": "FAIL", "message": "Issue"}
                        ],
                        "fixed": 0,
                        "manual": 0,
                        "dry_run_pending": 0,
                        "fixed_items": [],
                    },
                },
                timestamp=_ts_minutes_ago(5),
            ),
            _make_event(
                event_type="verify",
                category="asset_verify",
                result="succeeded",
                details={
                    "request": {"skill": None},
                    "result": {"passed": 5, "failed": 0},
                },
                timestamp=_ts_minutes_ago(10),
            ),
            _make_event(
                event_type="code_scan",
                category="code_scan",
                result="succeeded",
                details={
                    "request": {"code": "echo hi", "language": "bash"},
                    "result": {
                        "ok": True,
                        "verdict": "pass",
                        "summary": "OK",
                        "findings": [],
                        "language": "bash",
                        "engine_version": "0.1.0",
                        "elapsed_ms": 1,
                    },
                },
                timestamp=_ts_minutes_ago(15),
            ),
        ]
        output = format_summary(events, "last 24 hours")

        # Verify all sections present
        assert "Security Posture Summary (last 24 hours)" in output
        assert "--- Hardening ---" in output
        assert "--- Asset Verification ---" in output
        assert "--- Code Scanning ---" in output
        assert "Total events: 3" in output

        # Verify sections appear in correct order
        hardening_pos = output.index("--- Hardening ---")
        verify_pos = output.index("--- Asset Verification ---")
        code_pos = output.index("--- Code Scanning ---")
        assert hardening_pos < verify_pos < code_pos

    def test_combined_with_prompt_scan(self):
        """Prompt Scan section appears after Sandbox Guard when all categories present."""
        events = [
            _make_event(
                event_type="harden",
                category="hardening",
                result="succeeded",
                details={
                    "request": {"args": ["--scan"]},
                    "result": {
                        "mode": "scan",
                        "passed": 48,
                        "failed": 0,
                        "total": 48,
                        "failures": [],
                    },
                },
                timestamp=_ts_minutes_ago(5),
            ),
            _make_prompt_scan_event(
                "deny",
                threat_type="indirect_injection",
                summary="Injection attempt detected",
                minutes_ago=2,
            ),
        ]
        output = format_summary(events, "last 24 hours")

        assert "--- Hardening ---" in output
        assert "--- Prompt Scan ---" in output
        assert "Total events: 2" in output

        hardening_pos = output.index("--- Hardening ---")
        prompt_pos = output.index("--- Prompt Scan ---")
        assert hardening_pos < prompt_pos

        # DENY verdict should flip posture to Needs attention
        assert "Needs attention" in output


# ---------------------------------------------------------------------------
# Test: footer and suggestions
# ---------------------------------------------------------------------------


class TestFooter:
    def test_footer_stats(self):
        """Latest harden succeeded so posture not critical; footer shows stats."""
        events = [
            _make_event(
                event_type="harden",
                category="hardening",
                result="succeeded",
                details={
                    "request": {"args": ["--scan"]},
                    "result": {
                        "mode": "scan",
                        "passed": 48,
                        "failed": 0,
                        "total": 48,
                        "failures": [],
                    },
                },
                timestamp=_ts_minutes_ago(14),
            ),
            _make_event(
                event_type="harden",
                category="hardening",
                result="failed",
                details={
                    "request": {"args": ["--scan"]},
                    "error": "timeout",
                    "error_type": "TimeoutError",
                },
                timestamp=_ts_minutes_ago(30),
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Total events: 2" in output
        assert "Failed: 1" in output
        # Latest event succeeded, so posture should be Good, not Critical
        assert "Good" in output

    def test_suggested_action_for_failed_rules(self):
        events = [
            _make_event(
                event_type="harden",
                category="hardening",
                result="succeeded",
                details={
                    "request": {"args": ["--scan"]},
                    "result": {
                        "mode": "scan",
                        "passed": 45,
                        "failed": 3,
                        "total": 48,
                        "failures": [
                            {"rule_id": "SEC-001", "status": "FAIL", "message": "issue"}
                        ],
                    },
                },
                timestamp=_ts_minutes_ago(5),
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "agent-sec-cli harden --reinforce" in output
        # Latest harden has failures -> needs_attention
        assert "Needs attention" in output

    def test_needs_attention_on_latest_harden_failures(self):
        """Latest successful scan with non-empty failures triggers needs_attention."""
        events = [
            _make_event(
                event_type="harden",
                category="hardening",
                result="succeeded",
                details={
                    "request": {"args": ["--scan"]},
                    "result": {
                        "mode": "scan",
                        "passed": 45,
                        "failed": 3,
                        "total": 48,
                        "failures": [{"rule_id": "SEC-001"}],
                    },
                },
                timestamp=_ts_minutes_ago(5),
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Needs attention" in output
        assert "\u26a0" in output

    def test_needs_attention_on_latest_verify_failures(self):
        """Latest verify with failed > 0 triggers needs_attention."""
        events = [
            _make_event(
                event_type="verify",
                category="asset_verify",
                result="succeeded",
                details={"request": {}, "result": {"passed": 3, "failed": 1}},
                timestamp=_ts_minutes_ago(5),
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Needs attention" in output
        assert "\u26a0" in output


# ---------------------------------------------------------------------------
# Helpers: skill-ledger event factory
# ---------------------------------------------------------------------------


def _make_skill_ledger_event(
    command: str,
    status: str = "pass",
    skill_dir: str = "/opt/skills/my-skill",
    result_extra: dict[str, Any] | None = None,
    event_result: str = "succeeded",
    minutes_ago: int = 5,
) -> SecurityEvent:
    """Build a skill_ledger SecurityEvent for tests."""
    result_data: dict[str, Any] = {"command": command, "status": status}
    if result_extra:
        result_data.update(result_extra)
    return _make_event(
        event_type="skill_ledger",
        category="skill_ledger",
        result=event_result,
        details={
            "request": {"command": command, "skill_dir": skill_dir},
            "result": result_data,
        },
        timestamp=_ts_minutes_ago(minutes_ago),
    )


# ---------------------------------------------------------------------------
# Test: skill-ledger summary section
# ---------------------------------------------------------------------------


class TestSkillLedgerSummary:
    def test_section_header_present(self):
        events = [_make_skill_ledger_event("check", "pass")]
        output = format_summary(events, "last 24 hours")
        assert "--- Skill Ledger ---" in output

    def test_check_counts(self):
        events = [
            _make_skill_ledger_event("check", "pass", minutes_ago=1),
            _make_skill_ledger_event(
                "check", "pass", skill_dir="/opt/skills/b", minutes_ago=2
            ),
            _make_skill_ledger_event(
                "check", "error", event_result="failed", minutes_ago=3
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Checks performed: 3 (succeeded: 2, failed: 1)" in output

    def test_certification_counts(self):
        events = [
            _make_skill_ledger_event(
                "certify",
                result_extra={"scanStatus": "pass", "versionId": "v000001"},
                minutes_ago=1,
            ),
            _make_skill_ledger_event(
                "certify",
                result_extra={"scanStatus": "warn", "versionId": "v000001"},
                skill_dir="/opt/skills/b",
                minutes_ago=2,
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Certifications:   2 (pass: 1, warn: 1)" in output

    def test_certification_counts_accept_event_verdict(self):
        events = [
            _make_skill_ledger_event(
                "certify",
                result_extra={"verdict": "pass", "version_id": "v000001"},
                minutes_ago=1,
            ),
            _make_skill_ledger_event(
                "certify",
                result_extra={"verdict": "warn", "version_id": "v000001"},
                skill_dir="/opt/skills/b",
                minutes_ago=2,
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Certifications:   2 (pass: 1, warn: 1)" in output

    def test_status_distribution(self):
        events = [
            _make_skill_ledger_event(
                "check", "pass", skill_dir="/opt/skills/a", minutes_ago=1
            ),
            _make_skill_ledger_event(
                "check", "pass", skill_dir="/opt/skills/b", minutes_ago=2
            ),
            _make_skill_ledger_event(
                "check", "drifted", skill_dir="/opt/skills/c", minutes_ago=3
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Skills tracked: 3" in output
        assert "drifted: 1" in output
        assert "pass: 2" in output

    def test_tampered_alert(self):
        events = [
            _make_skill_ledger_event(
                "check",
                "tampered",
                skill_dir="/opt/skills/evil",
                result_extra={"reason": "manifestHash does not match"},
                minutes_ago=1,
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Tampered (1):" in output
        assert "evil" in output
        assert "manifestHash does not match" in output

    def test_denied_alert(self):
        events = [
            _make_skill_ledger_event(
                "check",
                "deny",
                skill_dir="/opt/skills/risky",
                minutes_ago=1,
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Denied (1):" in output
        assert "risky" in output
        assert "high-risk findings" in output

    def test_deduplicates_to_latest_per_skill(self):
        """Multiple checks for the same skill: only the latest counts."""
        events = [
            _make_skill_ledger_event(
                "check", "pass", skill_dir="/opt/skills/a", minutes_ago=1
            ),
            _make_skill_ledger_event(
                "check", "drifted", skill_dir="/opt/skills/a", minutes_ago=10
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Skills tracked: 1" in output
        # Latest is pass (1 min ago), not drifted (10 min ago)
        assert "pass: 1" in output
        assert "drifted" not in output.split("Status:")[1].split("\n")[0]

    def test_no_skills_tracked_when_only_failed_checks(self):
        """Failed check events should not appear in skill status tracking."""
        events = [
            _make_skill_ledger_event(
                "check", "error", event_result="failed", minutes_ago=1
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Skills tracked" not in output


# ---------------------------------------------------------------------------
# Test: skill-ledger posture integration
# ---------------------------------------------------------------------------


class TestSkillLedgerPosture:
    def test_tampered_triggers_needs_attention(self):
        events = [
            _make_skill_ledger_event(
                "check", "tampered", skill_dir="/opt/skills/evil", minutes_ago=1
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Needs attention" in output
        assert "\u26a0" in output

    def test_deny_triggers_needs_attention(self):
        events = [
            _make_skill_ledger_event(
                "check", "deny", skill_dir="/opt/skills/bad", minutes_ago=1
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Needs attention" in output
        assert "\u26a0" in output

    def test_pass_does_not_trigger_needs_attention(self):
        events = [
            _make_skill_ledger_event(
                "check", "pass", skill_dir="/opt/skills/good", minutes_ago=1
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Good" in output
        assert "\u2713" in output

    def test_drifted_does_not_trigger_needs_attention(self):
        """Drifted is a warning, not a critical signal for posture."""
        events = [
            _make_skill_ledger_event(
                "check", "drifted", skill_dir="/opt/skills/s", minutes_ago=1
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Good" in output
        assert "\u2713" in output

    def test_failed_event_does_not_affect_posture(self):
        """A skill_ledger event that itself failed should NOT affect posture."""
        events = [
            _make_skill_ledger_event(
                "check",
                "tampered",
                event_result="failed",
                minutes_ago=1,
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Good" in output


# ---------------------------------------------------------------------------
# Test: skill-ledger suggestions
# ---------------------------------------------------------------------------


class TestSkillLedgerSuggestions:
    def test_tampered_suggestion(self):
        events = [
            _make_skill_ledger_event(
                "check", "tampered", skill_dir="/opt/skills/x", minutes_ago=1
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Investigate tampered skills" in output

    def test_drifted_suggestion(self):
        events = [
            _make_skill_ledger_event(
                "check", "drifted", skill_dir="/opt/skills/x", minutes_ago=1
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Re-scan drifted skills" in output

    def test_none_suggestion(self):
        events = [
            _make_skill_ledger_event(
                "check", "none", skill_dir="/opt/skills/x", minutes_ago=1
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "Scan unchecked skills" in output

    def test_no_suggestion_when_all_pass(self):
        events = [
            _make_skill_ledger_event(
                "check", "pass", skill_dir="/opt/skills/a", minutes_ago=1
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "skill-ledger" not in output.split("---\n")[-1]


# ---------------------------------------------------------------------------
# Test: skill-ledger in mixed report
# ---------------------------------------------------------------------------


class TestSkillLedgerMixed:
    def test_section_order_in_combined_report(self):
        """Skill Ledger section appears after Hardening."""
        events = [
            _make_event(
                event_type="harden",
                category="hardening",
                result="succeeded",
                details={
                    "request": {"args": ["--scan"]},
                    "result": {
                        "mode": "scan",
                        "passed": 48,
                        "failed": 0,
                        "total": 48,
                        "failures": [],
                    },
                },
                timestamp=_ts_minutes_ago(5),
            ),
            _make_skill_ledger_event(
                "check", "pass", skill_dir="/opt/skills/a", minutes_ago=3
            ),
        ]
        output = format_summary(events, "last 24 hours")
        assert "--- Hardening ---" in output
        assert "--- Skill Ledger ---" in output
        harden_pos = output.index("--- Hardening ---")
        ledger_pos = output.index("--- Skill Ledger ---")
        assert harden_pos < ledger_pos


# ---------------------------------------------------------------------------
# Test: pii_scan summary
# ---------------------------------------------------------------------------


class TestPiiScanSummary:
    def test_pii_scan_section_and_deny_posture(self):
        events = [
            _make_event(
                event_type="pii_scan",
                category="pii_scan",
                result="succeeded",
                details={
                    "request": {"source": "tool_output", "text_length": 32},
                    "result": {
                        "ok": True,
                        "verdict": "deny",
                        "summary": {
                            "total": 1,
                            "by_type": {"api_key": 1},
                            "by_category": {"credential": 1},
                            "by_severity": {"deny": 1},
                        },
                        "findings": [],
                    },
                },
            )
        ]

        output = format_summary(events, "last 24 hours")

        assert "System Status: Needs attention" in output
        assert "--- PII Scan ---" in output
        assert "Verdict breakdown: deny: 1" in output
        assert "Finding types: api_key: 1" in output
