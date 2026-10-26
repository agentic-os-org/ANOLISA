"""Task-local execution leases; native tools explicitly consume their environment."""

from __future__ import annotations

from contextlib import asynccontextmanager
from contextvars import ContextVar
from dataclasses import dataclass, field
import os
from typing import Any, AsyncIterator, Callable


@dataclass
class PrivateExecution:
    """Application-owned credentials never enter the worker's global environment.

    An application factory yields this lease and removes credential files in its
    async context manager's finally block. Tools use execution_environment() at
    their subprocess boundary. Hook environments deliberately exclude secrets.
    Redaction must be idempotent across repeated Hook and delivery boundaries.
    """

    environment: dict[str, str]
    hook_environment: dict[str, str]
    redact: Callable[[Any], Any]
    secrets: dict[str, str] = field(default_factory=dict)


current_execution: ContextVar[PrivateExecution | None] = ContextVar(
    "aw_qwenpaw_private_execution", default=None
)


def execution_environment() -> dict[str, str]:
    """Return a copy for a native tool's explicit env= subprocess argument."""
    lease = current_execution.get()
    if lease is None:
        raise RuntimeError("AW native execution has no task-private environment")
    return {**lease.environment, **lease.secrets}


@asynccontextmanager
async def public_execution() -> AsyncIterator[PrivateExecution]:
    environment = dict(os.environ)
    lease = PrivateExecution(environment, dict(environment), lambda value: value)
    try:
        yield lease
    finally:
        lease.environment.clear()
        lease.hook_environment.clear()
        lease.secrets.clear()
