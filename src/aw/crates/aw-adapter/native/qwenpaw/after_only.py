"""Compatibility entry for an after-only deployment over the common native guard."""

from aw_sdk import AwError
from .deferred import DeferredPlugin


class AfterOnlyPlugin(DeferredPlugin):
    """Retain the original after-only capability and policy restriction."""

    def __init__(
        self,
        *,
        endpoint: str,
        target: str = "qwenpaw",
        attach_timeout_ms: int = 5000,
        reconnect_ms: int = 100,
        cwd: str | None = None,
    ) -> None:
        super().__init__(
            endpoint=endpoint,
            target=target,
            attach_timeout_ms=attach_timeout_ms,
            reconnect_ms=reconnect_ms,
            cwd=cwd,
        )
        self.instance.capabilities["version"] = "installed-after-hook/v1"
        self.instance.capabilities["events"] = {"tool.after": ["observe", "replace_result"]}

    def ready(self, routes: dict) -> bool:
        ready = super().ready(routes)
        for hook in routes["hooks"]:
            if hook["event"] != "tool.after" and not (
                hook["event"] == "tool.before" and hook["step"] == "$aw.observe"
            ):
                raise AwError("after_only_policy_required")
        return ready
