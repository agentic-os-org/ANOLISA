"""Build a per-session security observability debrief."""

import json
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any

from agent_sec_cli.observability.sqlite_reader import ObservabilityReader


@dataclass
class SessionReport:
    session_id: str
    first_seen: str
    last_seen: str
    duration_seconds: float
    turn_count: int
    llm_calls: int
    request_bytes: int
    response_bytes: int
    tool_breakdown: dict[str, int] = field(default_factory=dict)
    security_verdicts: dict[str, dict[str, int]] = field(default_factory=dict)
    security_hint: str = ""

    def to_dict(self) -> dict[str, Any]:
        return {
            "session_id": self.session_id,
            "first_seen": self.first_seen,
            "last_seen": self.last_seen,
            "duration_seconds": round(self.duration_seconds, 1),
            "turn_count": self.turn_count,
            "llm_calls": self.llm_calls,
            "request_bytes": self.request_bytes,
            "response_bytes": self.response_bytes,
            "tool_breakdown": self.tool_breakdown,
            "security_verdicts": self.security_verdicts,
            "security_hint": self.security_hint or None,
        }


def _epoch_to_iso(epoch: float) -> str:
    return datetime.fromtimestamp(epoch, tz=timezone.utc).strftime("%Y-%m-%d %H:%M:%S")


def _parse_metrics(metrics_json: str | None) -> dict[str, Any]:
    if not metrics_json:
        return {}
    try:
        return json.loads(metrics_json)
    except (json.JSONDecodeError, TypeError):
        return {}


def build_session_report(
    session_id: str,
    obs_reader: ObservabilityReader,
    sec_reader: Any | None = None,
) -> SessionReport | None:
    sessions = obs_reader.list_sessions()
    session = next((s for s in sessions if s.session_id == session_id), None)
    if session is None:
        return None

    runs = obs_reader.list_runs(session_id)
    all_events = []
    for run in runs:
        all_events.extend(obs_reader.list_events(session_id, run.run_id))

    llm_calls = 0
    req_bytes = 0
    resp_bytes = 0
    tool_counts: dict[str, int] = {}

    for ev in all_events:
        metrics = _parse_metrics(ev.metrics_json)
        if ev.hook == "after_llm_call":
            llm_calls += 1
            req_bytes += int(metrics.get("request_payload_bytes", 0))
            resp_bytes += int(metrics.get("response_stream_bytes", 0))
        elif ev.hook == "before_tool_call":
            name = metrics.get("tool_name", "unknown")
            tool_counts[name] = tool_counts.get(name, 0) + 1

    _ALL_CATEGORIES = [
        "code_scan",
        "prompt_scan",
        "pii_scan",
        "skill_ledger",
        "sandbox",
        "hardening",
    ]
    security: dict[str, dict[str, int]] = {}
    security_hint = ""
    if sec_reader is None:
        security_hint = "security-events DB not found"
    else:
        try:
            candidates = sec_reader.query_correlation_candidates(
                session_id=session_id,
                categories=_ALL_CATEGORIES,
            )
            for c in candidates:
                ev = c.event
                cat = ev.category
                result = ev.result
                if cat not in security:
                    security[cat] = {}
                security[cat][result] = security[cat].get(result, 0) + 1
            if not security:
                security_hint = "security hooks may not pass session_id yet"
        except Exception:
            security_hint = "failed to query security events"

    return SessionReport(
        session_id=session_id,
        first_seen=_epoch_to_iso(session.first_seen_epoch),
        last_seen=_epoch_to_iso(session.last_seen_epoch),
        duration_seconds=session.last_seen_epoch - session.first_seen_epoch,
        turn_count=session.turn_count,
        llm_calls=llm_calls,
        request_bytes=req_bytes,
        response_bytes=resp_bytes,
        tool_breakdown=dict(sorted(tool_counts.items(), key=lambda x: -x[1])),
        security_verdicts=security,
        security_hint=security_hint,
    )


def format_text(report: SessionReport) -> str:
    lines = []
    dur = report.duration_seconds
    dur_str = f"{int(dur // 60)}m {int(dur % 60)}s" if dur >= 60 else f"{dur:.0f}s"
    lines.append(
        f"Session {report.session_id[:12]}  "
        f"({report.first_seen} — {report.last_seen}, "
        f"{dur_str}, {report.turn_count} turns)"
    )
    lines.append("")

    lines.append(f"  LLM calls:       {report.llm_calls}")
    if report.request_bytes or report.response_bytes:
        lines.append(
            f"  Payload:         {report.request_bytes:,} bytes sent, "
            f"{report.response_bytes:,} bytes received"
        )
    lines.append("")

    if report.tool_breakdown:
        parts = [f"{name}({cnt})" for name, cnt in report.tool_breakdown.items()]
        lines.append(f"  Tools used:      {', '.join(parts)}")
    else:
        lines.append("  Tools used:      (none)")
    lines.append("")

    if report.security_verdicts:
        lines.append("  Security:")
        for cat, verdicts in sorted(report.security_verdicts.items()):
            parts = [f"{v}: {c}" for v, c in sorted(verdicts.items())]
            lines.append(f"    {cat:<20} {', '.join(parts)}")
    else:
        msg = "(no security events)"
        if report.security_hint:
            msg += f" — {report.security_hint}"
        lines.append(f"  Security:        {msg}")

    return "\n".join(lines)
