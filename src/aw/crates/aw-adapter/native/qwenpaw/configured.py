"""Configuration-selected native input and tool protection over the resident guard."""

from aw_sdk.configuration import load_configuration
from .deferred import DeferredPlugin


def from_config(
    config_file: str,
    *,
    target: str | None = None,
    executable: str = "aw",
    attach_timeout_ms: int = 5000,
    reconnect_ms: int = 100,
    cwd: str | None = None,
) -> DeferredPlugin:
    """Install input capture only when the authoritative configuration enables it."""
    spec, target, endpoint = load_configuration(config_file, "qwenpaw", target, executable)
    return DeferredPlugin(
        endpoint=endpoint,
        target=target,
        attach_timeout_ms=attach_timeout_ms,
        reconnect_ms=reconnect_ms,
        cwd=cwd,
        input_enabled=spec["events"].get("input.submit", {}).get("enabled") is True,
    )
