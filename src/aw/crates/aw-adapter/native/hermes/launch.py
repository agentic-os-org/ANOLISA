"""Keep pinned native startup intact, then require AW registration before chat."""

from argparse import Namespace
from collections import Counter
from contextlib import contextmanager, nullcontext
import json
import math
import os
import subprocess
from pathlib import Path
import sys
from types import ModuleType
from typing import Any, Callable
from uuid import uuid4


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



def check_submission(launch: dict[str, Any], session: str, text: Any) -> str | None:
    """Check a quiet CLI text turn before the native conversation starts."""
    hooks = launch.get("submissions", [])
    if not hooks:
        return None
    if (type(text) is not str or not text.strip() or not isinstance(session, str)
            or not session.strip() or len(session.encode()) > 512):
        return "AW input.submit requires a text turn and bounded session identity"
    payload = {"hook_event_name": "AWInputSubmit", "session_id": session,
               "aw_turn_id": str(uuid4()), "cwd": str(Path.cwd()), "input_text": text}
    try:
        encoded = json.dumps(payload, ensure_ascii=False, allow_nan=False).encode()
    except (ValueError, UnicodeError):
        return "AW input.submit requires UTF-8 text"
    if len(encoded) > 256 * 1024:
        return "AW input.submit exceeds 256 KiB of encoded input"

    def invoke(hook: dict[str, Any], close: bool = False) -> subprocess.CompletedProcess:
        argv = [launch["binary"], "hook", "--binding", launch["binding"],
                "--adapter", "hermes", "--event", "input.submit", "--step", hook["step"],
                "--on-error", hook["on_error"]]
        if close:
            argv += ["--close-event", "true"]
        return subprocess.run(argv, input=encoded, capture_output=True,
                              timeout=4 if close else hook["timeout"], check=False)

    try:
        for hook in hooks:
            try:
                reply = invoke(hook)
                value = json.loads(reply.stdout)
                if reply.returncode == 2 and value.get("action") == "block":
                    return "Input blocked by AW policy"
                if reply.returncode != 0 or value != {}:
                    raise ValueError("invalid submission response")
            except (OSError, subprocess.TimeoutExpired, ValueError, AttributeError):
                print("AW input.submit callback failed; on_error=" + hook["on_error"],
                      file=sys.stderr)
                if hook["on_error"] == "block":
                    return "AW input check failed; submission blocked"
    finally:
        # Close joins scanner cancellation before the native turn can be dispatched.
        if not launch.get("reloadable"):
            reply = invoke(hooks[0], close=True)
            if reply.returncode != 0:
                raise RuntimeError("AW input event cleanup failed")
    return None

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
    skills = None
    skill_restore = None
    dispatch_restore = None
    if launch.get("reloadable") or any(hook.get("skill_check") for hook in launch["hooks"]):
        import aw_skill as skills
    original_directives = plugins._get_pre_tool_call_directive_details
    original_execution = middleware.run_tool_execution_middleware
    post_restore: tuple[ModuleType, Callable[..., None]] | None = None
    input_restore: tuple[ModuleType, Callable[..., Any], Callable[..., Any]] | None = None
    manager = get_plugin_manager()
    original_discovery = manager.discover_and_load
    executing = 0
    reload_restore = None
    reload_runtime = None

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
        with guarded_hooks(), (skills.invocation(tool_name, arguments, dynamic=launch.get("reloadable", False)) if skills else nullcontext()):
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
        nonlocal post_restore, skill_restore, dispatch_restore, input_restore, reload_restore, reload_runtime
        admit_reload = None
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
        if launch.get("reloadable") and reload_restore is None:
            import cli
            from aw_reload import Runtime
            reload_runtime = Runtime(launch, lambda: executing)
            admit_reload = reload_runtime
            original_chat = cli.HermesCLI.chat
            reload_restore = (cli.HermesCLI, original_chat)
            cli.HermesCLI.chat = lambda client, *args, **kwargs: reload_runtime.chat(
                original_chat, client, *args, **kwargs)
        if launch.get("submissions") and input_restore is None:
            import cli
            original_main = cli.main
            original_images = cli._route_single_query_images

            def quiet_main(**options: Any) -> Any:
                query = options.get("query")
                if (not options.get("quiet") or type(query) is not str or not query.strip()
                        or options.get("image")):
                    raise SystemExit("AW input.submit requires a quiet single-query text turn")
                return original_main(**options)

            def input_text(client: Any, query: Any, effective: Any,
                           images: Any, image_urls: Any) -> Any:
                require_hooks()
                if images or image_urls:
                    raise SystemExit("AW input.submit does not support image input")
                if reload_runtime is not None:
                    reload_runtime.admit_input()
                denied = check_submission(launch, client.session_id, effective)
                if denied is not None:
                    if reload_runtime is not None:
                        reload_runtime.cancel_input()
                    print(denied, file=sys.stderr)
                    raise SystemExit(2)
                return original_images(client, query, effective, images, image_urls)

            input_restore = (cli, original_main, original_images)
            cli.main = quiet_main
            cli._route_single_query_images = input_text
        if launch["hooks"] and post_restore is None:
            # Importing model_tools discovers plugins; retain native startup order.
            import model_tools
            original_post = model_tools._emit_post_tool_call_hook
            if skills is not None:
                skill_restore = skills.install()
                dispatch_restore = (model_tools, model_tools.handle_function_call)
                original_dispatch = model_tools.handle_function_call

                def skill_dispatch(function_name, function_args, *values, **options):
                    with skills.invocation(function_name, function_args, dynamic=launch.get("reloadable", False)):
                        return original_dispatch(function_name, function_args, *values, **options)

                model_tools.handle_function_call = skill_dispatch

            def post(**context: Any) -> None:
                # Vetoes skip execution middleware and may have no live listeners.
                with guarded_hooks():
                    original_post(**context)

            post_restore = (model_tools, original_post)
            model_tools._emit_post_tool_call_hook = post
        if admit_reload is not None:
            admit_reload.ready()

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
    session.skills = skills
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
        if dispatch_restore is not None:
            dispatch_restore[0].handle_function_call = dispatch_restore[1]
        if skill_restore is not None:
            skill_restore()
        manager.discover_and_load = original_discovery
        if reload_restore is not None:
            reload_restore[0].chat = reload_restore[1]
        if input_restore is not None:
            cli, original_main, original_images = input_restore
            cli.main = original_main
            cli._route_single_query_images = original_images
        sys.modules.pop(session.__name__, None)


if __name__ == "__main__":
    main()
