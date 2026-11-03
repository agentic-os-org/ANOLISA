"""Keep pinned native startup intact, then require AW registration before chat."""

from argparse import Namespace
from collections import Counter
from contextlib import contextmanager
import json
import math
import os
from pathlib import Path
import sys
from types import ModuleType
from typing import Any, Callable


def _json_native(value: Any) -> bool:
    if type(value) is dict:
        return all(type(key) is str and _json_native(key) and _json_native(child)
                   for key, child in value.items())
    if type(value) is list:
        return all(_json_native(child) for child in value)
    if type(value) is int:
        # serde_json otherwise rounds overflowing integers into floating-point values.
        return -(1 << 63) <= value < (1 << 64)
    if type(value) is float:
        return math.isfinite(value)
    if type(value) is str:
        try:
            value.encode("utf-8")
        except UnicodeEncodeError:
            return False
        return True
    return type(value) in (bool, type(None))


def main() -> None:
    launch_path = os.environ["AW_HERMES_LAUNCH"]
    # The private launch directory keeps imports off unverified profile caches.
    sys.pycache_prefix = str(Path(launch_path).parent / "pycache")
    # Native main resolves the profile and loads its dotenv before dispatch.
    sys.argv = sys.argv[1:]
    from hermes_cli import main as native
    # Profile dotenv values cannot redirect this launch's binding before discovery.
    os.environ["AW_HERMES_LAUNCH"] = launch_path
    from hermes_cli import middleware, plugins
    from hermes_cli.plugins import discover_plugins, get_plugin_manager
    from utils import env_var_enabled

    original = native._prepare_agent_startup
    launch = json.loads(Path(launch_path).read_text())
    original_directives = plugins._get_pre_tool_call_directive_details
    original_execution = middleware.run_tool_execution_middleware
    post_restore: tuple[ModuleType, Callable[..., None]] | None = None
    manager = get_plugin_manager()
    original_discovery = manager.discover_and_load
    executing = 0

    def discovery(force: bool = False) -> None:
        if not force:
            original_discovery()
            return
        # Native teardown must not remove the audit callbacks of an in-flight tool.
        with manager._discovery_lock:
            if executing:
                raise RuntimeError("Hermes plugin reload during AW tool execution is unsupported")
            original_discovery(force=force)
    rewrite_error = "Hermes tool argument modification is unsupported with AW before callbacks"

    before = any(hook["event"] == "tool.before" for hook in launch["hooks"])

    @contextmanager
    def guarded_hooks() -> Any:
        nonlocal executing
        with manager._discovery_lock:
            executing += 1
        try:
            require_hooks()
            yield
        finally:
            with manager._discovery_lock:
                executing -= 1

    def directives(*values: Any, **options: Any) -> plugins._PreToolCallDirective:
        with guarded_hooks():
            details = original_directives(*values, **options)
            # Native callbacks inspect the original arguments; their rewrites are
            # merged only afterward. Reject rewrites rather than execute unchecked input.
            if before and details.modified_args is not None:
                return plugins._PreToolCallDirective(
                    action="block", message=details.message if details.action == "block" else
                    rewrite_error,
                    rule_key=details.rule_key,
                )
            return details

    def execution(tool_name: str, arguments: dict[str, Any],
                  next_call: Callable[[dict[str, Any]], Any], **context: Any) -> Any:
        with guarded_hooks():
            # Freeze JSON values before middleware, including nested mutations and
            # type changes that Python equality would treat as unchanged.
            inspected = json.dumps(arguments, sort_keys=True) if before else None

            def dispatch(effective: dict[str, Any]) -> Any:
                require_hooks()
                if before:
                    from tools.registry import tool_error
                    if not _json_native(effective):
                        return tool_error("Hermes tool arguments contain unsupported JSON values")
                    if json.dumps(effective, sort_keys=True) != inspected:
                        return tool_error(rewrite_error)
                return next_call(effective)

            return original_execution(tool_name, arguments, dispatch, **context)

    def prepare(args: Namespace) -> None:
        nonlocal post_restore
        if env_var_enabled("HERMES_SAFE_MODE"):
            raise SystemExit("HERMES_SAFE_MODE disables the AW native plugin")
        original(args)
        # Join native background discovery without changing registration order.
        discover_plugins()
        expected = {"version": 1, "adapter": "hermes", "token": launch["token"],
                    "pid": os.getpid(), "hooks": len(launch["hooks"])}
        try:
            receipt = json.loads(Path(launch["ready_path"]).read_text())
        except (OSError, ValueError):
            raise SystemExit(
                "Hermes AW callbacks were not registered before chat; "
                "check plugin selections and rerun aw install"
            ) from None
        if receipt != expected or env_var_enabled("HERMES_SAFE_MODE"):
            raise SystemExit("Hermes AW registration does not match this launch")
        require_hooks()
        if launch["hooks"] and post_restore is None:
            # Importing model_tools discovers plugins; retain native startup order.
            import model_tools
            original_post = model_tools._emit_post_tool_call_hook

            def post(**context: Any) -> None:
                # Vetoes skip execution middleware and may have no live listeners.
                with guarded_hooks():
                    original_post(**context)

            post_restore = (model_tools, original_post)
            model_tools._emit_post_tool_call_hook = post

    def require_hooks() -> None:
        # Join in-flight native discovery before examining its ownership ledger.
        discover_plugins()
        manager = get_plugin_manager()
        plugin_path = Path(launch["profile"]) / "plugins" / "aw-native-hooks"
        loaded = [(key, plugin) for key, plugin in manager._plugins.items()
                  if plugin.manifest.path and Path(plugin.manifest.path).resolve() == plugin_path]
        if len(loaded) != 1 or not loaded[0][1].enabled or loaded[0][1].error:
            raise SystemExit(
                "Hermes AW plugin is not loaded and enabled; "
                "check plugin selections and rerun aw install"
            )
        key, plugin = loaded[0]
        events = {"tool.before": "pre_tool_call", "tool.after": "post_tool_call"}
        expected_hooks = Counter("transform_tool_result" if hook.get("replace_result") else events[hook["event"]] for hook in launch["hooks"])
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
    # Keep this guard outside plugin teardown so native rediscovery cannot remove it.
    if launch["hooks"]:
        plugins._get_pre_tool_call_directive_details = directives
        middleware.run_tool_execution_middleware = execution
        manager.discover_and_load = discovery
    try:
        native.main()
    finally:
        plugins._get_pre_tool_call_directive_details = original_directives
        middleware.run_tool_execution_middleware = original_execution
        if post_restore is not None:
            module, callback = post_restore
            module._emit_post_tool_call_hook = callback
        manager.discover_and_load = original_discovery
        sys.modules.pop(session.__name__, None)


if __name__ == "__main__":
    main()
