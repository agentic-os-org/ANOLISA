"""Daemon health method."""

import os
from typing import Any

from agent_sec_cli.daemon.protocol import DaemonRequest
from agent_sec_cli.daemon.registry import (
    HandlerResult,
    MethodRegistry,
    MethodSpec,
)
from agent_sec_cli.daemon.runtime import DaemonRuntime

_PROMPT_SCAN_UNTRACKED_DETAIL = (
    "Prompt scanning runs in-process in the Rust extension; the daemon does not "
    "observe scanner or L2 model state. Use `agent-sec-cli scan-prompt` output or "
    "`agent-sec-cli events --event-type prompt_scan` to assess real availability."
)


def build_health_snapshot(runtime: DaemonRuntime) -> dict[str, Any]:
    """Build the daemon.health response without initializing heavy modules."""
    return {
        "status": runtime.status,
        "pid": os.getpid(),
        "uptime_seconds": runtime.uptime_seconds(),
        "socket": str(runtime.socket_path),
        # Scanning moved in-process to the Rust extension, so the daemon has no
        # scanner state to report. The legacy PromptScanRuntimeState keys stay
        # present so monitors reading `.prompt_scan.*` keep parsing, but every
        # availability-bearing field is null/unknown: reporting ready/loaded=true
        # here would mask an unusable L2 layer behind a green probe. `tracked`
        # lets probes reject this payload instead of treating it as healthy.
        "prompt_scan": {
            "status": "unknown",
            "model": None,
            "loaded": None,
            "last_error": None,
            "last_started_at": None,
            "last_finished_at": None,
            "tracked": False,
            "detail": _PROMPT_SCAN_UNTRACKED_DETAIL,
        },
        "jobs": runtime.jobs.status(),
        "queues": runtime.queues.to_dict(),
    }


def health_handler(_request: DaemonRequest, runtime: DaemonRuntime) -> HandlerResult:
    """Return daemon runtime health."""
    return HandlerResult(data=build_health_snapshot(runtime))


def register_health_methods(registry: MethodRegistry) -> None:
    """Register daemon health methods."""
    registry.register(
        MethodSpec(
            method="daemon.health",
            handler=health_handler,
            lifecycle="admin",
            queue="admin",
            timeout_ms=1000,
            access_log=False,
        )
    )
