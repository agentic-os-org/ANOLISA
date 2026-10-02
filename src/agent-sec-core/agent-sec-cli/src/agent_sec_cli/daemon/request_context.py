"""Daemon request context normalization and request-local scope."""

from collections.abc import Iterator
from contextlib import contextmanager
from contextvars import ContextVar

from agent_sec_cli.correlation_context import (
    TraceContext,
    parse_trace_context_payload,
    reset_current_trace_context,
    set_current_trace_context,
    trace_context_to_payload,
)
from agent_sec_cli.daemon.protocol import DaemonRequest

_CURRENT_DAEMON_REQUEST_ID: ContextVar[str | None] = ContextVar(
    "daemon_request_id",
    default=None,
)


def normalize_request_trace_context(
    request: DaemonRequest,
) -> tuple[DaemonRequest, TraceContext]:
    """Return *request* with normalized caller-provided trace context.

    Daemon ingress paths should call this before dispatch so access logs and
    downstream middleware share the same caller-provided correlation fields.
    """
    trace_context = parse_trace_context_payload(request.trace_context) or TraceContext()
    payload = trace_context_to_payload(trace_context)
    normalized_trace_context = parse_trace_context_payload(payload) or TraceContext()
    return (
        DaemonRequest(
            method=request.method,
            request_id=request.request_id,
            params=request.params,
            trace_context=payload,
            caller=request.caller,
            timeout_ms=request.timeout_ms,
        ),
        normalized_trace_context,
    )


@contextmanager
def daemon_request_context(
    trace_context: TraceContext,
    request_id: str | None = None,
) -> Iterator[None]:
    """Set request-local tracing context for one daemon request."""
    trace_token = set_current_trace_context(trace_context)
    request_token = _CURRENT_DAEMON_REQUEST_ID.set(request_id)
    try:
        yield
    finally:
        _CURRENT_DAEMON_REQUEST_ID.reset(request_token)
        reset_current_trace_context(trace_token)


def get_current_daemon_request_id() -> str | None:
    """Return the current daemon request id, when running inside a request."""
    return _CURRENT_DAEMON_REQUEST_ID.get()
