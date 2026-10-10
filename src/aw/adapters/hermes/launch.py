"""Keep pinned native startup intact, then require AW registration before chat."""

from collections import Counter
import json
import os
from pathlib import Path
import sys
from types import ModuleType


def main() -> None:
    launch_path = os.environ["AW_HERMES_LAUNCH"]
    # The private launch directory keeps imports off unverified profile caches.
    sys.pycache_prefix = str(Path(launch_path).parent / "pycache")
    # Native main resolves the profile and loads its dotenv before dispatch.
    sys.argv = sys.argv[1:]
    from hermes_cli import main as native
    # Profile dotenv values cannot redirect this launch's binding before discovery.
    os.environ["AW_HERMES_LAUNCH"] = launch_path
    from hermes_cli.plugins import discover_plugins, get_plugin_manager
    from utils import env_var_enabled

    original = native._prepare_agent_startup

    def prepare(args) -> None:
        if env_var_enabled("HERMES_SAFE_MODE"):
            raise SystemExit("HERMES_SAFE_MODE disables the AW native plugin")
        original(args)
        # Join native background discovery without changing registration order.
        discover_plugins()
        launch = json.loads(Path(launch_path).read_text())
        expected = {"version": 1, "adapter": "hermes", "token": launch["token"],
                    "pid": os.getpid(), "hooks": len(launch["hooks"])}
        try:
            receipt = json.loads(Path(launch["ready_path"]).read_text())
        except (OSError, ValueError):
            raise SystemExit("Hermes AW callbacks were not registered before chat") from None
        if receipt != expected or env_var_enabled("HERMES_SAFE_MODE"):
            raise SystemExit("Hermes AW registration does not match this launch")
        manager = get_plugin_manager()
        plugin_path = Path(launch["profile"]) / "plugins" / "aw-native-hooks"
        loaded = [(key, plugin) for key, plugin in manager._plugins.items()
                  if plugin.manifest.path and Path(plugin.manifest.path).resolve() == plugin_path]
        if len(loaded) != 1 or not loaded[0][1].enabled or loaded[0][1].error:
            raise SystemExit("Hermes AW plugin is not loaded and enabled")
        key, plugin = loaded[0]
        events = {"tool.before": "pre_tool_call", "tool.after": "post_tool_call"}
        expected_hooks = Counter(events[hook["event"]] for hook in launch["hooks"])
        active_hooks = Counter(registration.key
                               for registration in manager._ownership_ledger.get(key, [])
                               if registration.kind == "hook" and registration.active)
        if active_hooks != expected_hooks or Counter(plugin.hooks_registered) != expected_hooks:
            raise SystemExit("Hermes AW callbacks are not active in the native registry")

    native._prepare_agent_startup = prepare
    # Hermes reloads both plugin modules and dotenv during chat. Keep the binding
    # in this launcher's namespace, outside native plugin teardown and environment.
    session = ModuleType("_aw_hermes_session")
    session.launch_path = launch_path
    sys.modules[session.__name__] = session
    try:
        native.main()
    finally:
        sys.modules.pop(session.__name__, None)


if __name__ == "__main__":
    main()
