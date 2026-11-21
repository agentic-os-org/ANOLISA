"""Claude's last-mile binding for application-owned SDK workers.

The public SDK Task carries the resident producer contract; this module only
declares Claude's profile and capability ceiling, validates route installation
with the resident Bridge and selects Claude's per-event drain ownership.
"""

from aw_sdk.task_scope import TaskScope

# A Hook contract identity, not a package version: deployment descriptors are
# compiled offline and the same profile covers every admitted SDK release.
PROFILE = {"adapter": "claude-sdk", "version": "resident-hooks/v1", "entrypoint": "sdk-worker"}


def capabilities(input_enabled: bool) -> dict:
    """Capability ceiling installed by the resident Bridge for this worker."""
    events = {
        "session.start": ["observe"],
        "turn.stop": ["observe", "block"],
        "session.end": ["observe"],
        "tool.before": ["observe", "block", "replace_input"],
        "tool.after": ["observe", "replace_result"],
    }
    if input_enabled:
        events = {"input.submit": ["observe", "block"], **events}
    return {**PROFILE, "events": events}


def validate_routes(routes: dict) -> bool:
    """Reject a policy snapshot the resident Bridge cannot install."""
    from .aw_claude import Bridge

    Bridge({"hooks": routes["hooks"]}, launch_bound=False)
    return True


class SdkScope(TaskScope):
    """Resident Claude Scope carried by the public SDK Task."""

    async def invoke(self, hook: dict, payload: dict) -> tuple[int, bytes]:
        from .aw_scope import diagnostics

        # Claude drains its sequenced turn in the submission finally block.
        code, stdout, stderr = await self.invoke_native(
            hook, payload, drain_on_failure=hook["event"] != "input.submit"
        )
        diagnostics(stderr)
        return code, stdout
