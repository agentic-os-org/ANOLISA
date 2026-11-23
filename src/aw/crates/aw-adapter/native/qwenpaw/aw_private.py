"""Task-local execution leases; native tools explicitly consume their environment."""

from __future__ import annotations

from contextlib import asynccontextmanager
from contextvars import ContextVar
from dataclasses import dataclass, field
import os
from typing import Any, AsyncIterator, Callable


PRIVATE_EXECUTION_VERSION = "aw.private-execution/v1"


@dataclass(repr=False)
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
    version: str = PRIVATE_EXECUTION_VERSION
    _retired: bool = field(default=False, init=False)


def validate_execution(lease: PrivateExecution) -> None:
    """Admit the versioned contract before any native dispatch or Hook spawn.

    Copy the application dictionaries so subsequent application mutations cannot
    change the admitted environment. Declared secret keys and values must be
    absent from the Hook view; applications declare all sensitive values here.
    """
    if (not isinstance(lease, PrivateExecution) or lease.version != PRIVATE_EXECUTION_VERSION
            or lease._retired):
        raise RuntimeError("AW unsupported private execution contract")
    if not callable(lease.redact):
        raise RuntimeError("AW private execution requires a synchronous redactor")
    for name in ("environment", "hook_environment", "secrets"):
        value = getattr(lease, name)
        if not isinstance(value, dict) or any(
            not isinstance(key, str) or not key or "=" in key or "\0" in key
            or not isinstance(item, str) or "\0" in item
            for key, item in value.items()
        ):
            raise RuntimeError("AW invalid private execution environment")
        setattr(lease, name, dict(value))
    if any(key in lease.hook_environment for key in lease.secrets) or any(
        secret and secret in value
        for secret in lease.secrets.values()
        for pair in lease.hook_environment.items() for value in pair
    ):
        raise RuntimeError("AW private execution Hook environment contains a declared secret")


def clear_execution(lease: Any) -> None:
    """Retire owned dictionary references after the application's finalizer exits."""
    if isinstance(lease, PrivateExecution):
        lease._retired = True
        for name in ("environment", "hook_environment", "secrets"):
            value = getattr(lease, name)
            if isinstance(value, dict):
                value.clear()


current_execution: ContextVar[PrivateExecution | None] = ContextVar(
    "aw_qwenpaw_private_execution", default=None
)


def execution_environment() -> dict[str, str]:
    """Return a copy for a native tool's explicit env= subprocess argument."""
    lease = current_execution.get()
    if lease is None or lease._retired:
        raise RuntimeError("AW native execution has no task-private environment")
    return {**lease.environment, **lease.secrets}


def hook_environment() -> dict[str, str]:
    """Return the explicitly admitted Hook view without task credentials."""
    lease = current_execution.get()
    if lease is None or lease._retired:
        raise RuntimeError("AW native execution has no task-private environment")
    return dict(lease.hook_environment)


@asynccontextmanager
async def public_execution() -> AsyncIterator[PrivateExecution]:
    environment = dict(os.environ)
    lease = PrivateExecution(environment, dict(environment), lambda value: value)
    try:
        yield lease
    finally:
        clear_execution(lease)
