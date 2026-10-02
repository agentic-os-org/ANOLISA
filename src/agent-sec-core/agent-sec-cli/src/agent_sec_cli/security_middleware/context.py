"""RequestContext — per-invocation context propagated through the call chain."""

import uuid
from dataclasses import dataclass
from datetime import datetime, timezone

from agent_sec_cli.correlation_context import (
    get_current_trace_context,
    get_invocation_id,
)


def _new_uuid() -> str:
    return str(uuid.uuid4())


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


@dataclass
class RequestContext:
    """Immutable context created at the start of every ``invoke()`` call.

    Attributes:
        action:      The requested action name (e.g. ``"sandbox_prehook"``).
        trace_id:    Correlation ID propagated to all ``SecurityEvent`` records.
                     Auto-generated UUID if not supplied.
        caller:      Identity of the caller (``"sandbox-guard"``, ``"cli"``, …).
        session_id:  Optional session-level correlation ID.
        run_id:      Optional agent run or turn correlation ID.
        call_id:     Optional LLM call correlation ID.
        tool_call_id: Optional tool call correlation ID.
        agent_name:  Optional agent runtime name for telemetry metadata.
        timestamp:   ISO-8601 timestamp of request creation.  Auto-filled.
        invocation_id: Process-wide CLI invocation ID. Auto-filled.
    """

    action: str
    trace_id: str = ""
    caller: str = ""
    session_id: str | None = None
    run_id: str | None = None
    call_id: str | None = None
    tool_call_id: str | None = None
    agent_name: str | None = None
    timestamp: str = ""
    invocation_id: str = ""

    def __post_init__(self) -> None:
        if not self.invocation_id:
            self.invocation_id = get_invocation_id()
        trace_ctx = get_current_trace_context()
        if trace_ctx is not None:
            if not self.trace_id and trace_ctx.trace_id:
                self.trace_id = trace_ctx.trace_id
            if self.session_id is None:
                self.session_id = trace_ctx.session_id
            if self.run_id is None:
                self.run_id = trace_ctx.run_id
            if self.call_id is None:
                self.call_id = trace_ctx.call_id
            if self.tool_call_id is None:
                self.tool_call_id = trace_ctx.tool_call_id
            if self.agent_name is None:
                self.agent_name = trace_ctx.agent_name
        if not self.trace_id:
            self.trace_id = _new_uuid()
        if not self.timestamp:
            self.timestamp = _now_iso()
