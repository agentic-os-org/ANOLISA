"""Start cooperative AW control before exposing a dedicated Hermes Gateway API."""

from pathlib import Path

from aw_sdk.transport import AwError


def listen(runner, *, config_file: str, descriptor_file: str, target: str | None = None,
           attach_timeout_ms: int = 5000):
    """Install first-tool admission on the running loop. Close after draining the Gateway."""
    from hermes_cli.plugins import discover_plugins, get_plugin_manager
    from hermes_constants import get_hermes_home

    if type(attach_timeout_ms) is not int or not 100 <= attach_timeout_ms <= 60000:
        raise AwError("invalid_attach_timeout")
    discover_plugins()
    manager = get_plugin_manager()
    expected = (Path(get_hermes_home()) / "plugins" / "aw-native-hooks").resolve()
    with manager._discovery_lock:
        plugins = [plugin for plugin in manager._plugins.values()
                   if plugin.manifest.path and Path(plugin.manifest.path).resolve() == expected]
        if len(plugins) != 1 or not plugins[0].enabled or plugins[0].error:
            raise AwError("hermes_plugin_unavailable")
        start = getattr(plugins[0].module, "start_sdk", None)
        if not callable(start):
            raise AwError("hermes_plugin_unavailable")
        return start(runner, config_file, descriptor_file, target, attach_timeout_ms)
