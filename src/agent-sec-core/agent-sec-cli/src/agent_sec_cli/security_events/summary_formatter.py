"""Human-readable security posture summary from SecurityEvent records.

Aggregates events by category and produces an actionable text report
suitable for CLI stdout or upstream consumer display.
"""

from collections import defaultdict
from datetime import datetime, timezone
from pathlib import PurePosixPath
from typing import Any

from agent_sec_cli.security_events.schema import SecurityEvent

# ---------------------------------------------------------------------------
# Public entry point
# ---------------------------------------------------------------------------


def format_summary(events: list[SecurityEvent], time_label: str) -> str:
    """Produce a human-readable summary from a list of security events.

    Parameters
    ----------
    events : list[SecurityEvent]
        Pre-queried events (ordering not required; sorted internally).
    time_label : str
        Human label for the time window (e.g., "last 24 hours").

    Returns
    -------
    str
        Formatted multi-section summary text.
    """
    if not events:
        return "No security events recorded.\n"

    by_category = _group_by_category(events)
    sections: list[str] = []

    harden_events = by_category.get("hardening", [])
    asset_events = by_category.get("asset_verify", [])
    code_scan_events = by_category.get("code_scan", [])
    sandbox_events = by_category.get("sandbox", [])
    prompt_scan_events = by_category.get("prompt_scan", [])
    pii_scan_events = by_category.get("pii_scan", [])
    skill_ledger_events = by_category.get("skill_ledger", [])

    if harden_events:
        sections.append(_summarize_hardening(harden_events))
    if asset_events:
        sections.append(_summarize_asset_verify(asset_events))
    if code_scan_events:
        sections.append(_summarize_code_scan(code_scan_events))
    if sandbox_events:
        sections.append(_summarize_sandbox(sandbox_events))
    if prompt_scan_events:
        sections.append(_summarize_prompt_scan(prompt_scan_events))
    if pii_scan_events:
        sections.append(_summarize_pii_scan(pii_scan_events))
    if skill_ledger_events:
        sections.append(_summarize_skill_ledger(skill_ledger_events))

    ledger_statuses = (
        _skill_ledger_latest_statuses(skill_ledger_events)
        if skill_ledger_events
        else {}
    )

    header = _compute_posture(
        harden_events,
        asset_events,
        prompt_scan_events,
        pii_scan_events,
        ledger_statuses,
        time_label,
    )
    footer = _build_footer(events, harden_events, ledger_statuses)
    return "\n\n".join([header, *sections, footer])


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _group_by_category(events: list[SecurityEvent]) -> dict[str, list[SecurityEvent]]:
    """Group events into a dict keyed by category, newest-first."""
    by_category: dict[str, list[SecurityEvent]] = defaultdict(list)
    for e in events:
        by_category[e.category].append(e)
    # Ensure each group is sorted newest-first regardless of input order.
    for cat in by_category:
        by_category[cat].sort(key=lambda e: e.timestamp, reverse=True)
    return by_category


def _safe_details(event: SecurityEvent) -> dict[str, Any]:
    """Return event.details safely, defaulting to empty dict."""
    return event.details if isinstance(event.details, dict) else {}


def _get_result(event: SecurityEvent) -> dict[str, Any]:
    """Extract details.result dict from an event."""
    d = _safe_details(event)
    result = d.get("result")
    return result if isinstance(result, dict) else {}


def _get_request(event: SecurityEvent) -> dict[str, Any]:
    """Extract details.request dict from an event."""
    d = _safe_details(event)
    request = d.get("request")
    return request if isinstance(request, dict) else {}


def _is_full_verify(event: SecurityEvent) -> bool:
    """Check if a verify event is a full-skill verification.

    Full verify events have skill=None in the request.
    Single-skill verify events have a specific skill path or name.
    """
    request = _get_request(event)
    # Full verify when skill key is absent or explicitly None
    return request.get("skill") is None


def _asset_verify_outcome(event: SecurityEvent) -> str:
    """Return the semantic verification outcome, including for legacy events."""
    result = _get_result(event)
    outcome = result.get("outcome")
    if outcome in {"verified", "failed", "no_candidates"}:
        return str(outcome)

    if event.result == "failed":
        return "failed"

    passed = result.get("passed", 0)
    failed = result.get("failed", 0)
    if isinstance(failed, int) and failed > 0:
        return "failed"
    if passed == 0 and failed == 0:
        return "no_candidates"
    return "verified"


def _get_mode(event: SecurityEvent) -> str:
    """Extract hardening mode from details.result, fallback to parsing request.args.

    The mode field is written by HardeningBackend._build_result_data into
    ActionResult.data, which lifecycle.post_action stores as details.result.
    The CLI passes raw args (e.g. ["--scan", "--config", ...]) as
    details.request.args, so we parse those as a fallback.
    """
    result = _get_result(event)
    mode = result.get("mode")
    if mode:
        return mode
    # Fallback: parse request.args for --scan/--reinforce/--dry-run
    args = _get_request(event).get("args", [])
    if isinstance(args, (list, tuple)):
        if "--dry-run" in args:
            return "dry-run"
        if "--reinforce" in args:
            return "reinforce"
        if "--scan" in args:
            return "scan"
    return ""


def _has_hardening_stats(event: SecurityEvent) -> bool:
    """Return whether a hardening event has parsed rule summary statistics."""
    result = _get_result(event)
    return isinstance(result.get("total"), int) and result.get("total", 0) > 0


def _has_actionable_hardening_failure(event: SecurityEvent) -> bool:
    """Return whether a hardening event has a parsed rule failure to fix."""
    result = _get_result(event)
    failures = result.get("failures", [])
    if not isinstance(failures, list):
        return False

    for failure in failures:
        if not isinstance(failure, dict):
            continue
        if failure.get("status") == "UNKNOWN":
            continue
        if not failure.get("rule_id"):
            continue
        return True
    return False


def _format_timestamp(ts: str) -> str:
    """Render an ISO-8601 timestamp in local time for inline display."""
    try:
        dt = datetime.fromisoformat(ts)
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        return dt.astimezone().strftime("%Y-%m-%d %H:%M:%S")
    except (ValueError, TypeError):
        return ts


# ---------------------------------------------------------------------------
# Per-category formatters
# ---------------------------------------------------------------------------


def _summarize_hardening(events: list[SecurityEvent]) -> str:
    """Summarize hardening category events."""
    lines = ["--- Hardening ---"]

    # Classify by mode in a single pass (mode lives in details.result)
    scans: list[SecurityEvent] = []
    reinforcements: list[SecurityEvent] = []
    for e in events:
        mode = _get_mode(e)
        if mode == "scan":
            scans.append(e)
        elif mode == "reinforce":
            reinforcements.append(e)

    scans_ok = sum(1 for e in scans if e.result == "succeeded")
    scans_fail = len(scans) - scans_ok
    lines.append(
        f"  Scans performed:  {len(scans)} (succeeded: {scans_ok}, failed: {scans_fail})"
    )

    if reinforcements:
        reinf_ok = sum(1 for e in reinforcements if e.result == "succeeded")
        reinf_fail = len(reinforcements) - reinf_ok
        lines.append(
            f"  Reinforcements:   {len(reinforcements)} "
            f"(succeeded: {reinf_ok}, failed: {reinf_fail})"
        )

    # Latest scan result details. Loongshield returns non-zero for non-compliant
    # scans, but the backend may still parse usable passed/total statistics.
    latest_scan = next((e for e in scans if _has_hardening_stats(e)), None)
    if latest_scan:
        result = _get_result(latest_scan)
        passed = result.get("passed", 0)
        total = result.get("total", 0)
        failures = result.get("failures", [])

        # Include fixed count from reinforce operations in compliance calculation
        fixed_count = 0
        for e in reinforcements:
            if _has_hardening_stats(e):
                reinf_result = _get_result(e)
                fixed_count += reinf_result.get("fixed", 0)

        # Compliance includes both passed and fixed items
        effective_passed = passed + fixed_count

        if total > 0:
            pct = effective_passed / total * 100
            lines.append("")
            lines.append("  Latest scan result:")
            if fixed_count > 0:
                lines.append(
                    f"    Compliance: {effective_passed}/{total} rules passed "
                    f"({passed} passed + {fixed_count} fixed, {pct:.1f}%)"
                )
            else:
                lines.append(
                    f"    Compliance: {passed}/{total} rules passed ({pct:.1f}%)"
                )

            if failures and fixed_count == 0:
                lines.append(
                    "    Check system status using `agent-sec-cli harden --scan`"
                )
    elif scans:
        # All scans failed — show the latest error so users aren't left in the dark
        latest_error = scans[0]
        error_msg = _safe_details(latest_error).get("error", "unknown error")
        lines.append("")
        lines.append(f"  Latest scan failed: {error_msg}")

    return "\n".join(lines)


def _summarize_asset_verify(events: list[SecurityEvent]) -> str:
    """Summarize asset_verify category events."""
    lines = ["--- Asset Verification ---"]

    outcomes = [_asset_verify_outcome(event) for event in events]
    verified_count = outcomes.count("verified")
    skipped_count = outcomes.count("no_candidates")
    failed_count = outcomes.count("failed")
    lines.append(
        f"  Verifications performed: {len(events)} "
        f"(verified: {verified_count}, skipped: {skipped_count}, failed: {failed_count})"
    )

    latest = events[0]
    result = _get_result(latest)
    passed = result.get("passed", 0)
    failed = result.get("failed", 0)
    checked = result.get("checked", passed + failed)
    outcome = _asset_verify_outcome(latest)
    lines.append("")
    lines.append("  Latest result:")
    lines.append(f"    {checked} checked, {passed} passed, {failed} failed")
    if outcome == "verified":
        lines.append("    Integrity status: ALL CLEAR")
    elif outcome == "no_candidates":
        lines.append("    Integrity status: NOT ASSESSED (no candidate skills)")
    else:
        lines.append("    Integrity status: FAILURES DETECTED")
        error_msg = _safe_details(latest).get("error")
        if error_msg:
            lines.append(f"    Latest error: {error_msg}")
        lines.append("    Check details using `agent-sec-cli verify`")

    return "\n".join(lines)


def _summarize_code_scan(events: list[SecurityEvent]) -> str:
    """Summarize code_scan category events."""
    lines = ["--- Code Scanning ---"]

    ok_count = 0
    verdict_counts: dict[str, int] = defaultdict(int)
    for e in events:
        if e.result == "succeeded":
            ok_count += 1
            result = _get_result(e)
            verdict = result.get("verdict", "unknown")
            verdict_counts[verdict] += 1
    fail_count = len(events) - ok_count
    lines.append(
        f"  Scans performed: {len(events)} (succeeded: {ok_count}, failed: {fail_count})"
    )

    if verdict_counts:
        parts = [f"{v}: {c}" for v, c in sorted(verdict_counts.items())]
        lines.append(f"  Verdict: {', '.join(parts)}")

    return "\n".join(lines)


def _summarize_sandbox(events: list[SecurityEvent]) -> str:
    """Summarize sandbox category events."""
    lines = ["--- Sandbox Guard ---"]
    total = len(events)
    lines.append(f"  Total interventions: {total}")

    return "\n".join(lines)


def _summarize_prompt_scan(events: list[SecurityEvent]) -> str:
    """Summarize prompt_scan category events."""
    lines = ["--- Prompt Scan ---"]

    ok_count = 0
    verdict_counts: dict[str, int] = defaultdict(int)
    threat_type_counts: dict[str, int] = defaultdict(int)
    latest_threats: list[SecurityEvent] = []

    for e in events:
        if e.result == "succeeded":
            ok_count += 1
            result = _get_result(e)
            verdict = result.get("verdict", "unknown")
            verdict_counts[verdict] += 1
            if result.get("verdict") in ("warn", "deny"):
                threat_type = result.get("threat_type", "unknown")
                threat_type_counts[threat_type] += 1
                if len(latest_threats) < 3:
                    latest_threats.append(e)
    fail_count = len(events) - ok_count

    lines.append(
        f"  Scans performed: {len(events)} (succeeded: {ok_count}, failed: {fail_count})"
    )

    if verdict_counts:
        parts = [f"{v}: {c}" for v, c in sorted(verdict_counts.items())]
        lines.append(f"  Verdict breakdown: {', '.join(parts)}")

    if threat_type_counts:
        threat_parts = [f"{t}: {c}" for t, c in sorted(threat_type_counts.items())]
        lines.append(f"  Threat types: {', '.join(threat_parts)}")

    if latest_threats:
        lines.append("")
        lines.append(f"  Latest threat{'s' if len(latest_threats) > 1 else ''}:")
        for e in latest_threats:
            result = _get_result(e)
            verdict = result.get("verdict", "?").upper()
            threat_type = result.get("threat_type", "unknown")
            summary = result.get("summary", "")
            ts = _format_timestamp(e.timestamp)
            lines.append(f"    [{ts}] {verdict} — {threat_type}: {summary}")

    return "\n".join(lines)


def _summarize_pii_scan(events: list[SecurityEvent]) -> str:
    """Summarize pii_scan category events."""
    lines = ["--- PII Scan ---"]

    ok_count = 0
    verdict_counts: dict[str, int] = defaultdict(int)
    type_counts: dict[str, int] = defaultdict(int)

    for e in events:
        if e.result == "succeeded":
            ok_count += 1
            result = _get_result(e)
            verdict_counts[result.get("verdict", "unknown")] += 1
            summary = result.get("summary", {})
            by_type = summary.get("by_type", {}) if isinstance(summary, dict) else {}
            if isinstance(by_type, dict):
                for pii_type, count in by_type.items():
                    if isinstance(count, int):
                        type_counts[str(pii_type)] += count

    fail_count = len(events) - ok_count
    lines.append(
        f"  Scans performed: {len(events)} (succeeded: {ok_count}, failed: {fail_count})"
    )

    if verdict_counts:
        parts = [f"{v}: {c}" for v, c in sorted(verdict_counts.items())]
        lines.append(f"  Verdict breakdown: {', '.join(parts)}")

    if type_counts:
        parts = [f"{t}: {c}" for t, c in sorted(type_counts.items())]
        lines.append(f"  Finding types: {', '.join(parts)}")

    return "\n".join(lines)


def _summarize_skill_ledger(events: list[SecurityEvent]) -> str:
    """Summarize skill_ledger category events.

    Classifies events by command (check / certify), deduplicates check results
    to the latest per skill, and surfaces tampered / deny alerts.
    """
    lines = ["--- Skill Ledger ---"]

    # Classify events by command in a single pass
    checks: list[SecurityEvent] = []
    certifications: list[SecurityEvent] = []
    for e in events:
        result = _get_result(e)
        cmd = result.get("command", "")
        if cmd == "check":
            checks.append(e)
        elif cmd == "certify":
            certifications.append(e)

    # --- Check activity ---
    checks_ok = sum(1 for e in checks if e.result == "succeeded")
    checks_fail = len(checks) - checks_ok
    lines.append(
        f"  Checks performed: {len(checks)} "
        f"(succeeded: {checks_ok}, failed: {checks_fail})"
    )

    # --- Certification activity ---
    if certifications:
        cert_ok = sum(1 for e in certifications if e.result == "succeeded")
        scan_status_counts: dict[str, int] = defaultdict(int)
        for e in certifications:
            if e.result == "succeeded":
                result = _get_result(e)
                ss = result.get("verdict", result.get("scanStatus", "unknown"))
                scan_status_counts[ss] += 1
        parts = [f"{s}: {c}" for s, c in sorted(scan_status_counts.items())]
        lines.append(f"  Certifications:   {cert_ok} ({', '.join(parts)})")

    # --- Latest status per skill (deduplicate by skill_dir) ---
    latest_per_skill: dict[str, SecurityEvent] = {}
    for e in checks:
        if e.result != "succeeded":
            continue
        req = _get_request(e)
        skill_dir = req.get("skill_dir", "")
        if skill_dir and skill_dir not in latest_per_skill:
            latest_per_skill[skill_dir] = e

    if latest_per_skill:
        status_counts: dict[str, int] = defaultdict(int)
        tampered_skills: list[tuple[str, SecurityEvent]] = []
        denied_skills: list[tuple[str, SecurityEvent]] = []

        for skill_dir, e in latest_per_skill.items():
            s = _get_result(e).get("status", "unknown")
            status_counts[s] += 1
            skill_name = PurePosixPath(skill_dir).name
            if s == "tampered":
                tampered_skills.append((skill_name, e))
            elif s == "deny":
                denied_skills.append((skill_name, e))

        lines.append("")
        lines.append(f"  Skills tracked: {len(latest_per_skill)}")
        parts = [f"{s}: {c}" for s, c in sorted(status_counts.items())]
        lines.append(f"  Status: {', '.join(parts)}")

        # Critical alerts (capped at 3 each, matching prompt_scan pattern)
        if tampered_skills:
            lines.append("")
            lines.append(f"  Tampered ({len(tampered_skills)}):")
            for skill_name, e in tampered_skills[:3]:
                reason = _get_result(e).get("reason", "signature mismatch")
                ts = _format_timestamp(e.timestamp)
                lines.append(f"    [{ts}] {skill_name} — {reason}")

        if denied_skills:
            lines.append("")
            lines.append(f"  Denied ({len(denied_skills)}):")
            for skill_name, e in denied_skills[:3]:
                ts = _format_timestamp(e.timestamp)
                lines.append(f"    [{ts}] {skill_name} — high-risk findings")

    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Skill-ledger helpers for posture / suggestions
# ---------------------------------------------------------------------------


def _skill_ledger_latest_statuses(
    events: list[SecurityEvent],
) -> dict[str, int]:
    """Derive per-skill latest status counts from skill_ledger events.

    Returns a dict mapping status string to count (e.g. {"pass": 3, "tampered": 1}).
    Only considers succeeded *check* events, deduplicated to the newest per skill.
    """
    seen: set[str] = set()
    counts: dict[str, int] = defaultdict(int)
    for e in events:
        if e.result != "succeeded":
            continue
        result = _get_result(e)
        if result.get("command") != "check":
            continue
        skill_dir = _get_request(e).get("skill_dir", "")
        if not skill_dir or skill_dir in seen:
            continue
        seen.add(skill_dir)
        counts[result.get("status", "unknown")] += 1
    return dict(counts)


# ---------------------------------------------------------------------------
# Posture and footer
# ---------------------------------------------------------------------------


def _compute_posture(
    hardening_events: list[SecurityEvent],
    verify_events: list[SecurityEvent],
    prompt_scan_events: list[SecurityEvent],
    pii_scan_events: list[SecurityEvent],
    ledger_statuses: dict[str, int],
    time_label: str,
) -> str:
    """Compute overall security posture status.

    Status is determined by the latest hardening, asset_verify,
    prompt_scan, and skill_ledger results.
    """

    needs_attention = False

    # --- Hardening (latest event) ---
    if hardening_events:
        latest_harden = hardening_events[0]  # events ordered desc
        if latest_harden.result == "failed":
            needs_attention = True
        elif latest_harden.result == "succeeded":
            result = _get_result(latest_harden)
            failures = result.get("failures", [])
            if failures:
                needs_attention = True

    # --- Asset Verification (latest conclusive FULL verify event) ---
    # Only consider full-skill verifications (skill=None) for posture calculation
    # Single-skill and no-candidate runs must not change the prior system status.
    latest_full_verify = next(
        (
            event
            for event in verify_events
            if _is_full_verify(event)
            and _asset_verify_outcome(event) != "no_candidates"
        ),
        None,
    )
    if latest_full_verify and _asset_verify_outcome(latest_full_verify) == "failed":
        needs_attention = True

    # --- Prompt Scan (any DENY verdict) ---
    for e in prompt_scan_events:
        if e.result == "succeeded":
            result = _get_result(e)
            if result.get("verdict") == "deny":
                needs_attention = True
                break

    # --- PII Scan (any DENY verdict) ---
    for e in pii_scan_events:
        if e.result == "succeeded":
            result = _get_result(e)
            if result.get("verdict") == "deny":
                needs_attention = True
                break

    # --- Skill Ledger (any tampered or deny status) ---
    if ledger_statuses.get("tampered", 0) > 0 or ledger_statuses.get("deny", 0) > 0:
        needs_attention = True

    # Determine status
    if needs_attention:
        status_line = "System Status: Needs attention \u26a0"
    else:
        status_line = "System Status: Good \u2713"

    lines = [
        f"Security Posture Summary ({time_label})",
        "",
        status_line,
    ]
    return "\n".join(lines)


def _build_footer(
    events: list[SecurityEvent],
    hardening_events: list[SecurityEvent],
    ledger_statuses: dict[str, int],
) -> str:
    """Build footer with stats and suggested actions."""
    total = len(events)
    failed = sum(1 for e in events if e.result == "failed")

    # Find the newest event in O(n) instead of sorting
    newest = max(events, key=lambda e: e.timestamp) if events else None
    last_event_str = _time_since_last_event(newest) if newest else "N/A"

    lines = [
        "---",
        f"Total events: {total}  |  Failed: {failed}  |  Last event: {last_event_str}",
    ]

    # Suggested actions
    suggestions = _compute_suggestions(hardening_events, ledger_statuses)
    if suggestions:
        lines.append("")
        lines.append("Suggested actions:")
        for s in suggestions:
            lines.append(f"  {s}")

    return "\n".join(lines)


def _time_since_last_event(event: SecurityEvent) -> str:
    """Compute human-readable time since the given event."""
    try:
        event_dt = datetime.fromisoformat(event.timestamp)
        now = datetime.now(timezone.utc)
        delta = now - event_dt
        minutes = int(delta.total_seconds() / 60)
        if minutes < 1:
            return "just now"
        if minutes < 60:
            return f"{minutes} min ago"
        hours = minutes // 60
        if hours < 24:
            return f"{hours}h ago"
        days = hours // 24
        return f"{days}d ago"
    except (ValueError, TypeError):
        return "unknown"


def _compute_suggestions(
    hardening_events: list[SecurityEvent],
    ledger_statuses: dict[str, int],
) -> list[str]:
    """Generate actionable suggestions based on latest events."""
    suggestions: list[str] = []

    # --- Hardening suggestions ---
    if hardening_events:
        latest = hardening_events[0]  # newest-first after _group_by_category sort
        if _has_actionable_hardening_failure(latest) and (
            latest.result == "succeeded" or _has_hardening_stats(latest)
        ):
            suggestions.append("agent-sec-cli harden --reinforce    Fix failed rules")

    # --- Skill-ledger suggestions ---
    if ledger_statuses:
        _LEDGER_HINTS = [
            (
                "tampered",
                "agent-sec-cli skill-ledger check <dir>    Investigate tampered skills",
            ),
            (
                "drifted",
                "agent-sec-cli skill-ledger scan <dir>     Re-scan drifted skills",
            ),
            (
                "none",
                "agent-sec-cli skill-ledger scan <dir>     Scan unchecked skills",
            ),
        ]
        for status_key, hint in _LEDGER_HINTS:
            if ledger_statuses.get(status_key, 0) > 0:
                suggestions.append(hint)

    return suggestions
