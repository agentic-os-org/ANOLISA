"""Summary backend — security event aggregation and reporting."""

import time
from datetime import datetime, timezone
from typing import Any

from agent_sec_cli.security_events import get_reader
from agent_sec_cli.security_middleware.backends.base import BaseBackend
from agent_sec_cli.security_middleware.context import RequestContext
from agent_sec_cli.security_middleware.result import ActionResult


class SummaryBackend(BaseBackend):
    """Aggregates and reports security event statistics."""

    def execute(self, ctx: RequestContext, **kwargs: Any) -> ActionResult:
        """Query security events and produce a summary report."""
        hours: float = kwargs.get("hours", 24)
        category: str | None = kwargs.get("category", None)
        event_type: str | None = kwargs.get("event_type", None)

        # Compute time range
        now = time.time()
        since_epoch = now - hours * 3600
        since_iso = datetime.fromtimestamp(since_epoch, tz=timezone.utc).isoformat()
        until_iso = datetime.fromtimestamp(now, tz=timezone.utc).isoformat()

        # Query via reader
        reader = get_reader()
        total_events = reader.count(
            category=category,
            event_type=event_type,
            since=since_iso,
            until=until_iso,
        )
        by_category = reader.count_by(
            "category",
            category=category,
            event_type=event_type,
            since=since_iso,
            until=until_iso,
        )
        by_event_type = reader.count_by(
            "event_type",
            category=category,
            event_type=event_type,
            since=since_iso,
            until=until_iso,
        )

        # Build summary data
        data = {
            "total_events": total_events,
            "time_range": {"since": since_iso, "until": until_iso},
            "by_category": by_category,
            "by_event_type": by_event_type,
        }

        # Format stdout
        lines = [
            f"Security Events Summary (last {hours:.0f}h)",
            "=" * 40,
            f"Total events: {total_events}",
            "",
            "By category:",
        ]
        for cat, cnt in sorted(by_category.items(), key=lambda x: -x[1]):
            lines.append(f"  {cat}: {cnt}")
        if not by_category:
            lines.append("  (none)")
        lines.append("")
        lines.append("By event type:")
        for et, cnt in sorted(by_event_type.items(), key=lambda x: -x[1]):
            lines.append(f"  {et}: {cnt}")
        if not by_event_type:
            lines.append("  (none)")

        stdout = "\n".join(lines)

        return ActionResult(success=True, data=data, stdout=stdout)
