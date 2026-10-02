"""Daemon request validation extension points.

The gateway owns the validation hook because every ingress path should apply
the same request policy after it has produced a ``DaemonRequest``.  Concrete
validators are intentionally not implemented yet; future validators should
raise ``DaemonError`` subclasses so callers receive stable daemon responses.
"""

from typing import Protocol

from agent_sec_cli.daemon.protocol import DaemonRequest


class DaemonRequestValidator(Protocol):
    """Validate a normalized daemon request before it reaches a method handler."""

    def validate(self, request: DaemonRequest) -> None:
        """Validate *request* or raise a daemon error."""
        pass


class NoopDaemonRequestValidator:
    """Placeholder validator until daemon request policy is defined."""

    def validate(self, request: DaemonRequest) -> None:
        """Accept every request without additional validation."""
        pass
