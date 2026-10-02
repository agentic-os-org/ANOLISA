"""Allowlisted daemon method registry and dispatch."""

import asyncio
import inspect
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from typing import Any

from agent_sec_cli.daemon.errors import (
    DaemonError,
    DaemonTimeoutError,
    InternalDaemonError,
    UnknownMethodError,
)
from agent_sec_cli.daemon.protocol import (
    DEFAULT_TIMEOUT_MS,
    DaemonRequest,
    DaemonResponse,
    error_response,
    success_response,
)
from agent_sec_cli.daemon.runtime import DaemonRuntime


@dataclass(frozen=True)
class HandlerResult:
    """Normalized result returned by daemon method handlers."""

    data: Any = field(default_factory=dict)
    stdout: str = ""
    stderr: str = ""
    exit_code: int = 0


HandlerReturn = (
    HandlerResult | dict[str, Any] | Awaitable[HandlerResult | dict[str, Any]]
)
Handler = Callable[[DaemonRequest, DaemonRuntime], HandlerReturn]


@dataclass(frozen=True)
class MethodSpec:
    """Daemon method policy metadata and handler."""

    method: str
    handler: Handler
    lifecycle: str
    queue: str = "default"
    timeout_ms: int = 5000
    access_log: bool = True


class MethodRegistry:
    """Allowlist registry for daemon methods."""

    def __init__(self) -> None:
        self._methods: dict[str, MethodSpec] = {}

    def register(self, spec: MethodSpec) -> None:
        """Register one daemon method."""
        if spec.method in self._methods:
            raise ValueError(f"duplicate daemon method: {spec.method}")
        self._methods[spec.method] = spec

    def get(self, method: str) -> MethodSpec:
        """Return a method spec or raise an allowlist error."""
        spec = self._methods.get(method)
        if spec is None:
            raise UnknownMethodError(method)
        return spec

    def methods(self) -> tuple[str, ...]:
        """Return registered method names."""
        return tuple(sorted(self._methods))


async def dispatch_request(
    request: DaemonRequest,
    registry: MethodRegistry,
    runtime: DaemonRuntime,
) -> DaemonResponse:
    """Dispatch a validated request through the allowlisted registry."""
    timeout_ms = request.timeout_ms or DEFAULT_TIMEOUT_MS
    try:
        spec = registry.get(request.method)
        timeout_ms = request.timeout_ms or spec.timeout_ms
        result = await asyncio.wait_for(
            _invoke_handler(spec, request, runtime),
            timeout=timeout_ms / 1000,
        )
        return success_response(
            request.request_id,
            data=result.data,
            stdout=result.stdout,
            stderr=result.stderr,
            exit_code=result.exit_code,
        )
    except asyncio.TimeoutError:
        return error_response(request.request_id, DaemonTimeoutError(timeout_ms))
    except DaemonError as exc:
        return error_response(request.request_id, exc)
    except Exception:
        return error_response(request.request_id, InternalDaemonError())


async def _invoke_handler(
    spec: MethodSpec,
    request: DaemonRequest,
    runtime: DaemonRuntime,
) -> HandlerResult:
    handler_result = spec.handler(request, runtime)
    if inspect.isawaitable(handler_result):
        handler_result = await handler_result

    if isinstance(handler_result, HandlerResult):
        return handler_result
    if isinstance(handler_result, dict):
        return HandlerResult(data=handler_result)

    raise InternalDaemonError("daemon handler returned an invalid result")
