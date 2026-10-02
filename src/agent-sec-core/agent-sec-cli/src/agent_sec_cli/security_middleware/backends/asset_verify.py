"""Asset-verify backend — delegates to the asset_verify package."""

from typing import Any

from agent_sec_cli.asset_verify import (
    format_verification_result,
    run_verification,
)
from agent_sec_cli.security_middleware.backends.base import BaseBackend
from agent_sec_cli.security_middleware.context import RequestContext
from agent_sec_cli.security_middleware.result import ActionResult


class AssetVerifyBackend(BaseBackend):
    """Verify skill integrity using the asset_verify module."""

    def execute(
        self,
        ctx: RequestContext,
        skill: str | None = None,
        **kwargs: Any,
    ) -> ActionResult:
        """Run verification for a single skill or all configured directories.

        Args:
            ctx:   Request context (unused beyond tracing).
            skill: Optional path to a single skill directory to verify.
                   When *None*, all directories from ``config.conf`` are scanned.
        """
        try:
            results = run_verification(skill)
        except Exception as exc:
            return ActionResult(
                success=False,
                error=f"Verification error: {exc}",
                exit_code=1,
                error_type=type(exc).__name__,
            )

        has_failures = results["outcome"] == "failed"

        return ActionResult(
            success=(not has_failures),
            stdout=format_verification_result(results),
            data={
                "outcome": results["outcome"],
                "checked": results["checked"],
                "passed": len(results["passed"]),
                "failed": len(results["failed"]),
            },
            exit_code=1 if has_failures else 0,
        )
