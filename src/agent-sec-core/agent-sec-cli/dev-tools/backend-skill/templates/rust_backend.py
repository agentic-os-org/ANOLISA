"""{backend_name} backend — delegates to Rust native module."""

from __future__ import annotations

import json

from agent_sec_cli._native import {action_name} as rust_{action_name}
from agent_sec_cli.security_middleware.result import ActionResult


class {BackendName}Backend:
    """Backend for {backend_name} — uses Rust native implementation."""

    def execute(self, ctx, **kwargs) -> ActionResult:
        """Execute the backend logic via Rust."""
        try:
            req = json.dumps(kwargs)
            resp_json = rust_{action_name}(req)
            resp = json.loads(resp_json)
            return ActionResult(
                success=True,
                data=resp,
                stdout=self._format_stdout(resp),
            )
        except Exception as exc:
            return ActionResult(success=False, error=f"Rust error: {exc}", exit_code=1)

    @staticmethod
    def _format_stdout(resp: dict) -> str:
        """Build human-readable output from the Rust response."""
        return json.dumps(resp, indent=2)
