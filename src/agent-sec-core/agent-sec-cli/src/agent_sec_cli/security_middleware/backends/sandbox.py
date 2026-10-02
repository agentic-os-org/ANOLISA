"""Sandbox prehook backend — log sandbox decisions, future: evaluate isolation correctness."""

from typing import Any

from agent_sec_cli.security_middleware.backends.base import BaseBackend
from agent_sec_cli.security_middleware.context import RequestContext
from agent_sec_cli.security_middleware.result import ActionResult


class SandboxBackend(BaseBackend):
    """Record sandbox decisions for auditing.

    Currently logging-only (the lifecycle layer handles event emission).
    Future: evaluate isolation policy correctness here.
    """

    def execute(
        self,
        ctx: RequestContext,
        decision: str = "",
        command: str = "",
        reasons: str = "",
        network_policy: str = "",
        cwd: str = "",
        **kwargs: Any,
    ) -> ActionResult:
        """Record sandbox decision.  Currently logging-only (via lifecycle).

        Future: evaluate isolation policy correctness here.
        """
        return ActionResult(
            success=True,
            data={
                "decision": decision,
                "command": command,
                "reasons": reasons,
                "network_policy": network_policy,
                "cwd": cwd,
            },
        )
