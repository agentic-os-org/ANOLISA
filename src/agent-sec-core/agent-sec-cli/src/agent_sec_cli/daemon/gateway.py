"""Daemon request gateway.

The gateway is transport-agnostic: Unix sockets, in-process callers, or future
ingress paths should all produce a ``DaemonRequest`` and then execute it here.
"""

import time
from dataclasses import dataclass
from typing import Any

from agent_sec_cli.correlation_context import TraceContext
from agent_sec_cli.daemon.errors import UnknownMethodError
from agent_sec_cli.daemon.logging import log_daemon_event
from agent_sec_cli.daemon.protocol import DaemonRequest, DaemonResponse
from agent_sec_cli.daemon.registry import MethodRegistry, dispatch_request
from agent_sec_cli.daemon.request_context import (
    daemon_request_context,
    normalize_request_trace_context,
)
from agent_sec_cli.daemon.runtime import DaemonRuntime
from agent_sec_cli.daemon.validation import (
    DaemonRequestValidator,
    NoopDaemonRequestValidator,
)


@dataclass(frozen=True)
class PreparedDaemonRequest:
    """Daemon request normalized for gateway execution."""

    request: DaemonRequest
    trace_context: TraceContext
    access_log: bool


class DaemonGateway:
    """Normalize, validate, log, and dispatch daemon requests."""

    def __init__(
        self,
        registry: MethodRegistry,
        runtime: DaemonRuntime,
        validator: DaemonRequestValidator | None = None,
    ) -> None:
        self.registry = registry
        self.runtime = runtime
        self.validator = (
            validator if validator is not None else NoopDaemonRequestValidator()
        )

    def prepare(self, request: DaemonRequest) -> PreparedDaemonRequest:
        """Normalize request context before execution or completion logging."""
        normalized_request, trace_context = normalize_request_trace_context(request)
        return PreparedDaemonRequest(
            request=normalized_request,
            trace_context=trace_context,
            access_log=_access_log_enabled(self.registry, normalized_request.method),
        )

    async def execute(self, prepared: PreparedDaemonRequest) -> DaemonResponse:
        """Run a prepared daemon request through validation and method dispatch."""
        began_request = False
        with daemon_request_context(
            prepared.trace_context, prepared.request.request_id
        ):
            if prepared.access_log:
                _log_request_started(
                    request_id=prepared.request.request_id,
                    method=prepared.request.method,
                    caller=prepared.request.caller,
                    trace_context=prepared.trace_context,
                )

            self.validator.validate(prepared.request)
            self.runtime.begin_request()
            began_request = True
            try:
                return await dispatch_request(
                    prepared.request,
                    self.registry,
                    self.runtime,
                )
            finally:
                if began_request:
                    self.runtime.end_request()

    def complete(
        self,
        prepared: PreparedDaemonRequest,
        response: DaemonResponse,
        started: float,
        bytes_in: int,
        bytes_out: int,
    ) -> None:
        """Emit gateway completion logging after the transport writes a response."""
        if prepared.access_log or not response.ok:
            _log_request_completion(
                request_id=prepared.request.request_id,
                method=prepared.request.method,
                caller=prepared.request.caller,
                response=response,
                started=started,
                bytes_in=bytes_in,
                bytes_out=bytes_out,
                trace_context=prepared.trace_context,
            )


def _access_log_enabled(registry: MethodRegistry, method: str) -> bool:
    try:
        return registry.get(method).access_log
    except UnknownMethodError:
        return True


def _log_request_completion(
    request_id: str,
    method: str | None,
    response: DaemonResponse,
    started: float,
    bytes_in: int,
    bytes_out: int,
    caller: str | None = None,
    trace_context: TraceContext | None = None,
) -> None:
    latency_ms = int((time.monotonic() - started) * 1000)
    error_code = None if response.error is None else response.error.get("code")
    data: dict[str, Any] = {
        "request_id": request_id,
        "method": method,
        "caller": caller,
        "ok": response.ok,
        "exit_code": response.exit_code,
        "error_code": error_code,
        "latency_ms": latency_ms,
        "queue_ms": 0,
        "bytes_in": bytes_in,
        "bytes_out": bytes_out,
    }
    log_daemon_event(
        event="daemon_request_completed",
        message="daemon request completed",
        data=data,
        request_id=request_id,
        trace_context=trace_context,
    )


def _log_request_started(
    request_id: str,
    method: str | None,
    caller: str | None = None,
    trace_context: TraceContext | None = None,
) -> None:
    data: dict[str, Any] = {
        "request_id": request_id,
        "method": method,
        "caller": caller,
    }
    log_daemon_event(
        event="daemon_request_started",
        message="daemon request started",
        data=data,
        request_id=request_id,
        trace_context=trace_context,
    )
