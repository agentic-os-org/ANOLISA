"""Configuration-selected native input and tool protection over the resident guard."""

from aw_sdk.configuration import load_configuration
from .deferred import PROFILE, DeferredPlugin


def from_config(
    config_file: str,
    *,
    target: str | None = None,
    descriptor_file: str | None = None,
    executable: str = "aw",
    attach_timeout_ms: int = 5000,
    reconnect_ms: int = 100,
    cwd: str | None = None,
) -> DeferredPlugin:
    """Install input capture only when the authoritative configuration enables it."""
    wiring = load_configuration(
        config_file,
        PROFILE,
        target,
        executable,
        descriptor_file=descriptor_file,
        requires_service=True,
    )
    return DeferredPlugin(
        endpoint=wiring["endpoint"],
        target=wiring["target"],
        attach_timeout_ms=attach_timeout_ms,
        reconnect_ms=reconnect_ms,
        cwd=cwd,
        input_enabled=wiring["input_enabled"],
    )
