"""Native startup with a preinstalled result boundary and cooperative attachment."""

from aw_sdk import AwError, Instance

PROFILE = {
    "adapter": "qwenpaw",
    "version": "installed-native-hooks/v1",
    "entrypoint": "app",
}


class DeferredPlugin:
    """Install native guards and admit at the configured input or first tool."""

    def __init__(
        self,
        *,
        endpoint: str,
        target: str = "qwenpaw",
        attach_timeout_ms: int = 5000,
        reconnect_ms: int = 100,
        cwd: str | None = None,
        input_enabled: bool = False,
    ) -> None:
        if type(attach_timeout_ms) is not int or not 100 <= attach_timeout_ms <= 60000:
            raise AwError("invalid_attach_timeout")
        self.input_enabled = input_enabled
        self.attach_timeout_ms = attach_timeout_ms
        self.registered = False
        self.instance = Instance(
            endpoint=endpoint,
            target=target,
            availability="required",
            capabilities={
                **PROFILE,
                "events": {
                    "turn.stop": ["observe"],
                    "tool.before": ["observe", "block"],
                    "tool.after": ["observe", "replace_result"],
                    **({"input.submit": ["observe", "block"]} if input_enabled else {}),
                },
                "failure_actions": {"tool.after": ["withhold_result"]},
            },
            install=self.ready,
            reconnect_ms=reconnect_ms,
            cwd=cwd,
        )

    def ready(self, routes: dict) -> bool:
        from .plugin import validate_hooks

        validate_hooks(routes)
        return self.registered

    def register(self, api) -> None:
        if self.registered:
            raise AwError("sdk_hooks_already_installed")
        from .aw_native import install_guard
        from .after_resident import DeferredResident
        from .plugin import AwToolMiddleware

        install_guard()
        api.register_middleware(
            lambda ctx, _config: DeferredResident(
                {"hooks": [], "deferred_plugin": self}, AwToolMiddleware, ctx
            ),
            priority=0,
        )
        self.registered = True
        api.register_startup_hook("aw-sdk-listen", self.startup)
        api.register_shutdown_hook("aw-sdk-close", self.instance.close)

    async def startup(self) -> None:
        # Listening does not connect, register or wait for either backend.
        self.instance.listen()
