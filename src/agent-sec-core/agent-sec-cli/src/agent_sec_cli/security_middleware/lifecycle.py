"""Lifecycle hooks — transparent pre/post/error logging via security_events."""

from typing import Any

from agent_sec_cli.security_events import SecurityEvent, log_event
from agent_sec_cli.security_middleware.backends.base import BaseBackend
from agent_sec_cli.security_middleware.context import RequestContext
from agent_sec_cli.security_middleware.result import ActionResult
from agent_sec_cli.telemetry import record_security_event_telemetry

# ---------------------------------------------------------------------------
# Action → SecurityEvent category mapping
# ---------------------------------------------------------------------------

_ACTION_CATEGORY: dict[str, str] = {
    "sandbox_prehook": "sandbox",
    "harden": "hardening",
    "verify": "asset_verify",
    "summary": "summary",
    "code_scan": "code_scan",
    "prompt_scan": "prompt_scan",
    "pii_scan": "pii_scan",
    "skill_ledger": "skill_ledger",
}


def _category_for(action: str) -> str:
    """Return the event category for *action*, defaulting to the action name."""
    return _ACTION_CATEGORY.get(action, action)


def _emit_event(ctx: RequestContext, event: SecurityEvent) -> None:
    """Best-effort emit of security event and derived telemetry."""
    try:
        log_event(event)
    except Exception:  # noqa: BLE001
        pass
    try:
        record_security_event_telemetry(event, ctx)
    except Exception:  # noqa: BLE001
        pass


# ---------------------------------------------------------------------------
# Hooks
# ---------------------------------------------------------------------------


def pre_action(ctx: RequestContext, kwargs: dict[str, Any]) -> None:
    """No-op — kept for future extensibility.

    Single-event model: we only emit one event per invocation, either on
    successful completion (post_action) or on failure (on_error).  Logging a
    separate ``<action>_request`` event here would produce two events per call,
    which conflicts with that policy.
    """
    # Intentionally empty — do not add log_event() here.


def post_action(
    ctx: RequestContext,
    result: ActionResult,
    kwargs: dict[str, Any],
    backend: BaseBackend,
) -> None:
    """Log the single completion event after the backend completes.

    Merges *kwargs* (request inputs) and *result.data* (backend outputs) into a
    single event so the full request/response context is captured in one record.
    """
    try:
        details = backend.build_event_details(result, kwargs)
        if not result.success and result.error_type:
            details["error_type"] = result.error_type
            details["exit_code"] = result.exit_code
        event = SecurityEvent(
            event_type=ctx.action,
            category=_category_for(ctx.action),
            result="succeeded" if result.success else "failed",
            details=details,
            trace_id=ctx.trace_id,
            session_id=ctx.session_id,
            run_id=ctx.run_id,
            call_id=ctx.call_id,
            tool_call_id=ctx.tool_call_id,
        )
        _emit_event(ctx, event)
    except Exception:  # noqa: BLE001
        pass


def on_error(
    ctx: RequestContext,
    exception: Exception,
    kwargs: dict[str, Any],
    backend: BaseBackend,
) -> None:
    """Log the single error event when the backend raises.

    Merges *kwargs* (request inputs) and error details into a single event so
    the full request context is captured alongside the failure.
    """
    try:
        details = backend.build_error_details(exception, kwargs)
        event = SecurityEvent(
            event_type=ctx.action,
            category=_category_for(ctx.action),
            result="failed",
            details=details,
            trace_id=ctx.trace_id,
            session_id=ctx.session_id,
            run_id=ctx.run_id,
            call_id=ctx.call_id,
            tool_call_id=ctx.tool_call_id,
        )
        _emit_event(ctx, event)
    except Exception:  # noqa: BLE001
        pass
