"""Exercise the adapter with the pinned Hermes dispatcher and an owned profile."""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import sys
import tempfile
import py_compile
import struct
import threading
import unittest
from unittest.mock import patch
from types import ModuleType, SimpleNamespace
from typing import Any, Callable, TYPE_CHECKING

if TYPE_CHECKING:
    from hermes_cli.plugins import PluginManager

sys.dont_write_bytecode = True


parser = argparse.ArgumentParser()
parser.add_argument("--source", type=Path, required=True)
parser.add_argument("--plugin", type=Path, required=True)
args = parser.parse_args()


class NativeContract(unittest.TestCase):
    def setUp(self) -> None:
        self.addCleanup(sys.modules.pop, "_aw_hermes_session", None)
        self.addCleanup(setattr, sys, "pycache_prefix", sys.pycache_prefix)
        sys.pycache_prefix = None
        self.directory = tempfile.TemporaryDirectory(prefix="aw-hermes-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.home = self.root / "profile"
        self.home.mkdir(mode=0o700)
        self.trace = self.root / "trace.jsonl"
        self.command = self.root / "callback"
        shutil.copyfile(Path(__file__).with_name("native_command.py"), self.command)
        self.command.chmod(0o700)
        self.environment = patch.dict(os.environ, {
            "HERMES_HOME": str(self.home), "AW_HERMES_TRACE": str(self.trace),
            "HERMES_ENABLE_PROJECT_PLUGINS": "0", "HERMES_SAFE_MODE": "0",
        })
        self.environment.start()
        self.addCleanup(self.environment.stop)
        from hermes_constants import set_hermes_home_override, reset_hermes_home_override
        token = set_hermes_home_override(self.home)
        self.addCleanup(reset_hermes_home_override, token)
        spec = importlib.util.spec_from_file_location("aw_hermes_plugin", args.plugin / "__init__.py")
        self.plugin = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.plugin)

    def launch(self, steps: list[tuple[str, str]]) -> Path:
        launch = self.root / "launch.json"
        launch.write_text(json.dumps({
            "schema": "aw-hermes/v1alpha1", "profile": str(self.home),
            "cwd": str(Path.cwd()), "binary": str(self.command),
            "binding": str(self.root / "binding.json"),
            "ready_path": str(self.root / "ready.json"), "token": "fixture-token",
            "hooks": [{"event": event, "step": step, "on_error": "report", "timeout": 3}
                      for event, step in steps],
        }))
        launch.chmod(0o600)
        os.environ["AW_HERMES_LAUNCH"] = str(launch)
        session = ModuleType("_aw_hermes_session")
        session.launch_path = str(launch)
        sys.modules[session.__name__] = session
        return launch

    def test_launch_guard_requires_receipt_before_chat_dispatch(self) -> None:
        self.launch([])
        guard_path = args.plugin / "launch.py"
        spec = importlib.util.spec_from_file_location("aw_hermes_launch", guard_path)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        from hermes_cli import main as native
        from hermes_logging import _reset_queued_handlers
        self.addCleanup(_reset_queued_handlers)
        reached = []
        def dispatch() -> None:
            native._prepare_agent_startup(SimpleNamespace())
            reached.append("tool")
        original_argv = sys.argv[:]
        self.addCleanup(setattr, sys, "argv", original_argv)
        sys.argv = [str(guard_path), "hermes", "chat", "--cli"]
        with (patch.object(native, "main", dispatch),
              patch.object(native, "_prepare_agent_startup", lambda _args: None),
              patch("hermes_cli.plugins.discover_plugins", lambda: None)):
            with self.assertRaisesRegex(SystemExit, "not registered before chat"):
                guard.main()
        self.assertEqual(reached, [])
        self.assertNotIn("_aw_hermes_session", sys.modules)

    def test_callback_timeout_uses_native_yaml_and_environment_semantics(self) -> None:
        from hermes_cli.plugins import _resolve_hook_callback_timeout
        os.environ["AW_HERMES_TIMEOUT"] = "80"
        for scalar, expected in (("1:20", 80), ("012", 10), ('"${AW_HERMES_TIMEOUT}"', 80),
                                 ("0", 0), ("-1", 30), ("1000", 600)):
            (self.home / "config.yaml").write_text(
                "plugins:\n  hook_callback_timeout: " + scalar + "\n")
            self.assertEqual(_resolve_hook_callback_timeout(), expected, scalar)
        os.environ.pop("AW_HERMES_TIMEOUT")

    def test_guard_keeps_launcher_binding_when_native_import_overwrites_environment(self) -> None:
        from hermes_cli import main as native
        from hermes_cli.plugins_manifest import parse_manifest_file
        from hermes_logging import _reset_queued_handlers
        self.addCleanup(_reset_queued_handlers)
        plugin_path = self.home / "plugins/aw-native-hooks"
        shutil.copytree(args.plugin, plugin_path)
        launch = self.launch([("tool.before", "observe")])
        stale = self.root / "stale-launch.json"
        value = json.loads(launch.read_text())
        value.update(token="stale-token", hooks=[], ready_path=str(self.root / "stale-ready.json"))
        stale.write_text(json.dumps(value))
        stale.chmod(0o600)
        manager = self.manager()
        manifest = parse_manifest_file(plugin_path / "plugin.yaml", plugin_path, "user", "")
        guard_path = args.plugin / "launch.py"
        spec = importlib.util.spec_from_file_location("aw_hermes_binding_guard", guard_path)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        reached = []
        def dispatch() -> None:
            native._prepare_agent_startup(SimpleNamespace())
            reached.append("chat")
        original_import = __import__
        def load_profile_environment(name: str, *values: Any, **options: Any) -> ModuleType:
            module = original_import(name, *values, **options)
            if name == "hermes_cli" and len(values) >= 3 and "main" in values[2]:
                os.environ["AW_HERMES_LAUNCH"] = str(stale)
            return module
        self.addCleanup(setattr, sys, "argv", sys.argv[:])
        sys.argv = [str(guard_path), "hermes", "chat", "--cli"]
        with (patch.object(native, "main", dispatch),
              patch.object(native, "_prepare_agent_startup", lambda _args: manager._load_plugin(manifest)),
              patch("hermes_cli.plugins.discover_plugins", lambda: None),
              patch("hermes_cli.plugins.get_plugin_manager", return_value=manager),
              patch("builtins.__import__", load_profile_environment)):
            guard.main()
        self.assertEqual(os.environ["AW_HERMES_LAUNCH"], str(launch))
        self.assertEqual(reached, ["chat"])
        self.assertNotIn("_aw_hermes_session", sys.modules)
        self.assertTrue(manager.has_hook("pre_tool_call"))
        self.assertEqual(json.loads((self.root / "ready.json").read_text())["token"], "fixture-token")
        self.assertFalse((self.root / "stale-ready.json").exists())

    def test_guard_ignores_forged_profile_bytecode(self) -> None:
        from hermes_cli import main as native
        from hermes_cli.plugins_manifest import parse_manifest_file
        from hermes_logging import _reset_queued_handlers
        self.addCleanup(_reset_queued_handlers)
        plugin_path = self.home / "plugins/aw-native-hooks"
        shutil.copytree(args.plugin, plugin_path)
        init = plugin_path / "__init__.py"
        cache = Path(importlib.util.cache_from_source(str(init)))
        cache.parent.mkdir(mode=0o775)
        cache.parent.chmod(0o775)
        marker = self.root / "forged-cache-executed"
        poison = self.root / "poison.py"
        poison.write_text("from pathlib import Path\nPath(" + repr(str(marker)) + ").touch()\n")
        py_compile.compile(str(poison), cfile=str(cache), doraise=True,
                           invalidation_mode=py_compile.PycInvalidationMode.TIMESTAMP)
        content = cache.read_bytes()
        info = init.stat()
        cache.write_bytes(content[:8] + struct.pack("<II", int(info.st_mtime), info.st_size) + content[16:])
        cache.chmod(0o664)
        manifest = parse_manifest_file(plugin_path / "plugin.yaml", plugin_path, "user", "")
        # Control: the official loader executes timestamp-valid cached code.
        self.manager()._load_directory_module(manifest)
        self.assertTrue(marker.exists())
        marker.unlink()
        self.launch([("tool.before", "observe")])
        manager = self.manager()
        guard_path = args.plugin / "launch.py"
        spec = importlib.util.spec_from_file_location("aw_hermes_cache_guard", guard_path)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        reached = []
        def dispatch() -> None:
            native._prepare_agent_startup(SimpleNamespace())
            reached.append("chat")
        original_argv = sys.argv[:]
        self.addCleanup(setattr, sys, "argv", original_argv)
        sys.argv = [str(guard_path), "hermes", "chat", "--cli"]
        with (patch.object(native, "main", dispatch),
              patch.object(native, "_prepare_agent_startup", lambda _args: manager._load_plugin(manifest)),
              patch("hermes_cli.plugins.discover_plugins", lambda: None),
              patch("hermes_cli.plugins.get_plugin_manager", return_value=manager)):
            guard.main()
        self.assertFalse(marker.exists())
        self.assertEqual(reached, ["chat"])
        self.assertTrue(manager.has_hook("pre_tool_call"))
        self.assertEqual(sys.pycache_prefix, str(self.root / "pycache"))

    def test_native_load_timeout_receipt_cannot_admit_chat_without_live_hooks(self) -> None:
        from hermes_cli import main as native
        from hermes_cli.plugins_manifest import parse_manifest_file
        from hermes_cli import plugins_loader
        from hermes_logging import _reset_queued_handlers
        self.addCleanup(_reset_queued_handlers)
        plugin_path = self.home / "plugins/aw-native-hooks"
        shutil.copytree(args.plugin, plugin_path)
        (self.home / "config.yaml").write_text("plugins:\n  load_timeout_seconds: 0.001\n")
        self.launch([("tool.before", "deny")])
        manager = self.manager()
        manifest = parse_manifest_file(plugin_path / "plugin.yaml", plugin_path, "user", "")
        self.assertIsNotNone(manifest)
        release = threading.Event()
        original_load = manager._load_directory_module
        previous_workers = set(plugins_loader._ABANDONED_LOADERS)
        def delayed_load(*values: Any, **options: Any) -> ModuleType:
            if not release.wait(timeout=5):
                raise TimeoutError("fixture release deadline")
            return original_load(*values, **options)
        with patch.object(manager, "_load_directory_module", delayed_load):
            try:
                manager._load_plugin(manifest)
            finally:
                release.set()
                workers = [worker for worker in plugins_loader._ABANDONED_LOADERS
                           if worker not in previous_workers]
                for worker in workers:
                    worker.join(timeout=5)
                    self.assertFalse(worker.is_alive())
        self.assertEqual(len(workers), 1)
        self.assertFalse(manager.list_plugins()[0]["enabled"])
        self.assertEqual(manager._hooks, {})
        ready = json.loads((self.root / "ready.json").read_text())
        self.assertEqual(ready["hooks"], 1)
        guard_path = args.plugin / "launch.py"
        spec = importlib.util.spec_from_file_location("aw_hermes_launch_timeout", guard_path)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        reached = []
        def dispatch() -> None:
            native._prepare_agent_startup(SimpleNamespace())
            reached.append("tool")
        original_argv = sys.argv[:]
        self.addCleanup(setattr, sys, "argv", original_argv)
        sys.argv = [str(guard_path), "hermes", "chat", "--cli"]
        with (patch.object(native, "main", dispatch),
              patch.object(native, "_prepare_agent_startup", lambda _args: None),
              patch("hermes_cli.plugins.discover_plugins", lambda: None),
              patch("hermes_cli.plugins.get_plugin_manager", return_value=manager)):
            with self.assertRaisesRegex(SystemExit, "not loaded and enabled"):
                guard.main()
        self.assertEqual(reached, [])

    def manager(self) -> PluginManager:
        from hermes_cli.plugins import PluginManager
        return PluginManager(scope_key=str(self.home))

    def context(self, manager: PluginManager) -> SimpleNamespace:
        def register_hook(name: str, callback: Callable[..., Any]) -> None:
            manager._hooks.setdefault(name, []).append(callback)
        return SimpleNamespace(register_hook=register_hook)

    def test_replacement_uses_the_final_native_transform_seam(self) -> None:
        launch = self.launch([("tool.after", "observe"), ("tool.after", "user"), ("tool.after", "compress")])
        config = json.loads(launch.read_text())
        config["hooks"][-1]["replace_result"] = True
        launch.write_text(json.dumps(config))
        manager = self.manager()
        self.plugin.register(self.context(manager))
        self.assertEqual(len(manager._hooks["post_tool_call"]), 2)
        self.assertIn("transform_tool_result", manager._hooks)
        import model_tools
        ids = model_tools._CallIds("task", "session", "call", "turn", "request")
        import subprocess
        original_run = subprocess.run
        def checked_run(*args, **kwargs):
            completed = original_run(*args, **kwargs)
            self.assertEqual(completed.returncode, 0, completed.stderr.decode(errors="replace"))
            return completed
        with (patch("hermes_cli.lifecycle.has_hook", lambda event: bool(manager._hooks.get(event))),
              patch("hermes_cli.lifecycle.invoke_hook", manager.invoke_hook),
              patch.object(self.plugin.subprocess, "run", checked_run)):
            model_tools._emit_post_tool_call_hook(function_name="terminal", function_args={"command":"echo text"},
                result='{"output":"original","exit_code":0}', duration_ms=1,
                task_id="task", session_id="session", tool_call_id="call", turn_id="turn", api_request_id="request")
            result = model_tools._apply_transform_tool_result_hook("terminal", {"command":"echo text"},
                '{"output":"original","exit_code":0}', 1, ids)
        self.assertEqual(result, "compact")
        rows = [json.loads(row) for row in self.trace.read_text().splitlines()]
        self.assertEqual([row["step"] for row in rows], ["observe", "user", "compress"])
        self.assertTrue(all(row["payload"]["extra"]["result"] == '{"output":"original","exit_code":0}' for row in rows))
        payload = rows[-1]["payload"]
        self.assertEqual(payload["hook_event_name"], "transform_tool_result")
        self.assertEqual(payload["extra"]["api_request_id"], "request")

    def test_native_order_and_block_keep_later_callbacks(self) -> None:
        self.launch([("tool.before", "deny"), ("tool.before", "after-deny")])
        manager = self.manager()
        self.plugin.register(self.context(manager))
        results = manager.invoke_hook(
            "pre_tool_call", tool_name="arbitrary_tool", args={"unicode": "猫", "nested": [1, False]},
            session_id="session", tool_call_id="call", custom={"field": 4},
        )
        self.assertEqual(results, [{"action": "block", "message": "native block"}])
        rows = [json.loads(line) for line in self.trace.read_text().splitlines()]
        self.assertEqual([row["step"] for row in rows], ["deny", "after-deny"])
        self.assertEqual(rows[0]["payload"], rows[1]["payload"])
        self.assertEqual(rows[0]["payload"]["extra"]["tool_call_id"], "call")
        self.assertEqual(rows[0]["payload"]["tool_input"]["unicode"], "猫")
        self.assertEqual(rows[0]["cwd"], str(Path.cwd()))

    def test_guard_rejects_native_rewrites_only_with_before_callbacks(self) -> None:
        from agent.shell_hooks import ShellHookSpec, _make_callback
        from hermes_cli import main as native, plugins
        from hermes_logging import _reset_queued_handlers
        self.addCleanup(_reset_queued_handlers)
        self.addCleanup(setattr, sys, "argv", sys.argv[:])
        plugin_path = self.home / "plugins/aw-native-hooks"
        shutil.copytree(args.plugin, plugin_path)
        (self.home / "config.yaml").write_text("plugins: {enabled: [aw-native-hooks]}\n")
        bundled = self.root / "bundled"
        bundled.mkdir()
        os.environ["HERMES_BUNDLED_PLUGINS"] = str(bundled)
        spec = importlib.util.spec_from_file_location("aw_hermes_rewrite_guard", args.plugin / "launch.py")
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        original_reducer = plugins._get_pre_tool_call_directive_details
        modifier = _make_callback(ShellHookSpec(
            event="pre_tool_call", timeout=3,
            command=shlex.join([str(self.command), "hook", "--step", "modify"]),
        ))
        for steps in ([("tool.before", "observe"), ("tool.after", "audit")],
                      [("tool.before", "deny"), ("tool.after", "audit")],
                      [("tool.after", "audit")], []):
            with self.subTest(steps=steps):
                (self.root / "ready.json").unlink(missing_ok=True)
                self.trace.unlink(missing_ok=True)
                self.launch(steps)
                manager = self.manager()
                self.addCleanup(manager.unload)
                sys.argv = [str(args.plugin / "launch.py"), "hermes", "chat", "--cli"]

                def dispatch() -> None:
                    native._prepare_agent_startup(SimpleNamespace())
                    for reloading in (False, True):
                        if reloading:
                            manager.discover_and_load(force=True)
                        for first in (True, False):
                            hooks = [hook for hook in manager._hooks.get("pre_tool_call", [])
                                     if hook is not modifier]
                            hooks.insert(0 if first else len(hooks), modifier)
                            manager._hooks["pre_tool_call"] = hooks
                            original = {"command": "printf approved", "retained": "猫"}
                            message, changed = plugins._dispatch_pre_tool_call_hooks(
                                "terminal", original, session_id="session", tool_call_id="call",
                                api_request_id="request",
                            )
                            if any(event == "tool.before" for event, _ in steps):
                                if steps[0][1] == "deny":
                                    self.assertEqual(message, "native block")
                                else:
                                    self.assertIn("modification is unsupported", message)
                                self.assertIsNone(changed)
                            else:
                                self.assertIsNone(message)
                                self.assertEqual(changed, {**original,
                                    "command": "printf rewritten > rewritten.txt"})
                            self.assertEqual(original["command"], "printf approved")
                            manager.invoke_hook("post_tool_call", tool_name="terminal",
                                args=changed or original, session_id="session", tool_call_id="call",
                                result='{"error":"blocked"}' if message else "ok",
                                status="blocked" if message else "success")

                with (patch.object(native, "main", dispatch),
                      patch.object(native, "_prepare_agent_startup", lambda _args: manager.discover_and_load()),
                      patch("hermes_cli.plugins.discover_plugins", manager.discover_and_load),
                      patch("hermes_cli.plugins.get_plugin_manager", return_value=manager)):
                    guard.main()
                self.assertIs(plugins._get_pre_tool_call_directive_details, original_reducer)
                rows = [json.loads(line) for line in self.trace.read_text().splitlines()]
                before = [row for row in rows if row["step"] in ("observe", "deny")]
                audit = [row for row in rows if row["step"] == "audit"]
                self.assertEqual(len(before), 4 if steps and steps[0][0] == "tool.before" else 0)
                self.assertEqual(len(audit), 4 if steps else 0)
                if before:
                    self.assertTrue(all(row["payload"]["tool_input"]["command"] == "printf approved"
                                        for row in before))
                    self.assertTrue(all(row["payload"]["extra"]["status"] == "blocked"
                                        for row in audit))
                self.assertFalse((Path.cwd() / "rewritten.txt").exists())

    def test_execution_middleware_cannot_rewrite_nested_code_calls(self) -> None:
        from hermes_cli import main as native, middleware, plugins
        from hermes_logging import _reset_queued_handlers
        from tools.code_kernel import CellAuthority
        from tools.registry import registry
        self.addCleanup(_reset_queued_handlers)
        self.addCleanup(setattr, sys, "argv", sys.argv[:])
        plugin_path = self.home / "plugins/aw-native-hooks"
        shutil.copytree(args.plugin, plugin_path)
        (self.home / "config.yaml").write_text("plugins: {enabled: [aw-native-hooks]}\n")
        bundled = self.root / "bundled"
        bundled.mkdir()
        os.environ["HERMES_BUNDLED_PLUGINS"] = str(bundled)
        spec = importlib.util.spec_from_file_location("aw_hermes_execution_guard", args.plugin / "launch.py")
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        original_execution = middleware.run_tool_execution_middleware
        for steps in ([("tool.before", "observe"), ("tool.after", "audit")],
                      [("tool.after", "audit")], []):
            with self.subTest(steps=steps):
                (self.root / "ready.json").unlink(missing_ok=True)
                self.trace.unlink(missing_ok=True)
                self.launch(steps)
                manager = self.manager()
                self.addCleanup(manager.unload)
                sys.argv = [str(args.plugin / "launch.py"), "hermes", "chat", "--cli"]
                executed = []

                def handler(_name: str, values: dict[str, Any], **_options: Any) -> str:
                    executed.append(json.loads(json.dumps(values)))
                    return '{"result":"executed"}'

                def dispatch() -> None:
                    native._prepare_agent_startup(SimpleNamespace())
                    for reloading in (False, True):
                        if reloading:
                            manager.discover_and_load(force=True)
                        for mode in ("replacement", "in-place", "type", "tuple", "integer-key",
                                     "unsigned-overflow", "signed-overflow", "nan", "infinite",
                                     "surrogate", "surrogate-key", "integer-bounds", "observe"):
                            def execution(args: dict[str, Any], next_call: Callable[..., Any],
                                          **_options: Any) -> Any:
                                if mode == "replacement":
                                    return next_call({**args, "command": "unchecked replacement"})
                                if mode == "in-place":
                                    args["nested"]["allowed"] = False
                                if mode == "type":
                                    args["flag"] = 1
                                if mode == "tuple":
                                    args["nested"]["items"] = tuple(args["nested"]["items"])
                                if mode == "integer-key":
                                    args["nested"]["mapping"] = {1: "approved"}
                                return next_call(dict(args))
                            manager._middleware["tool_execution"] = [execution]
                            executed.clear()
                            items = {
                                "unsigned-overflow": [(1 << 64) + 1],
                                "signed-overflow": [-(1 << 63) - 1],
                                "nan": [float("nan")], "infinite": [float("inf")],
                                "surrogate": ["\ud800"], "surrogate-key": [{"\ud800": "value"}],
                                "integer-bounds": [-(1 << 63), (1 << 64) - 1],
                            }.get(mode, [1])
                            result = CellAuthority("code-task").dispatch("terminal", {
                                "command": "approved", "nested": {
                                    "allowed": True, "items": items, "mapping": {"1": "approved"},
                                }, "flag": True,
                            })
                            blocked = bool(steps and steps[0][0] == "tool.before"
                                           and mode not in ("observe", "integer-bounds"))
                            if blocked:
                                self.assertEqual(executed, [])
                                error = ("modification is unsupported" if mode in
                                         ("replacement", "in-place", "type") else "unsupported JSON")
                                self.assertIn(error, json.loads(result)["error"])
                            else:
                                self.assertEqual(len(executed), 1)

                with (patch.object(native, "main", dispatch),
                      patch.object(native, "_prepare_agent_startup", lambda _args: manager.discover_and_load()),
                      patch("hermes_cli.plugins.discover_plugins", manager.discover_and_load),
                      patch("hermes_cli.plugins.get_plugin_manager", return_value=manager),
                      patch.object(registry, "dispatch", handler)):
                    guard.main()
                self.assertIs(middleware.run_tool_execution_middleware, original_execution)
                rows = [json.loads(line) for line in self.trace.read_text().splitlines()] if self.trace.exists() else []
                before = [row for row in rows if row["step"] == "observe"]
                audit = [row for row in rows if row["step"] == "audit"]
                self.assertEqual(len(before), 26 if steps and steps[0][0] == "tool.before" else 0)
                self.assertEqual(len(audit), 26 if steps else 0)
                self.assertTrue(all(row["payload"]["tool_input"]["command"] == "approved" for row in before))
                if before:
                    self.assertEqual([row["payload"]["extra"]["status"] for row in audit],
                                     (["error"] * 11 + ["ok"] * 2) * 2)

    def test_failed_forced_reload_cannot_dispatch_tools(self) -> None:
        from hermes_cli import main as native, plugins_loader
        from hermes_logging import _reset_queued_handlers
        from tools.code_kernel import CellAuthority
        from tools.registry import registry
        self.addCleanup(_reset_queued_handlers)
        self.addCleanup(setattr, sys, "argv", sys.argv[:])
        bundled = self.root / "bundled"
        bundled.mkdir()
        os.environ["HERMES_BUNDLED_PLUGINS"] = str(bundled)
        shutil.copytree(args.plugin, self.home / "plugins/aw-native-hooks")
        spec = importlib.util.spec_from_file_location("aw_reload_guard", args.plugin / "launch.py")
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        for steps in ([("tool.before", "deny"), ("tool.after", "audit")],
                      [("tool.after", "audit")], []):
            for failure in ("omitted", "timeout"):
                with self.subTest(steps=steps, failure=failure):
                    (self.root / "ready.json").unlink(missing_ok=True)
                    self.trace.unlink(missing_ok=True)
                    config = self.home / "config.yaml"
                    config.write_text('plugins: {enabled: [aw-native-hooks]}\n')
                    self.launch(steps)
                    manager = self.manager()
                    self.addCleanup(manager.unload)
                    sys.argv = [str(args.plugin / "launch.py"), "hermes", "chat", "--cli"]
                    executed = []

                    def handler(_name: str, values: dict[str, Any], **_options: Any) -> str:
                        executed.append(values)
                        return '{"result":"executed"}'

                    def dispatch() -> None:
                        native._prepare_agent_startup(SimpleNamespace())
                        receipt = (self.root / "ready.json").read_bytes()
                        if failure == "omitted":
                            config.write_text('plugins: {enabled: []}\n')
                            manager.discover_and_load(force=True)
                        else:
                            config.write_text('plugins: {enabled: [aw-native-hooks], load_timeout_seconds: 0.001}\n')
                            release = threading.Event()
                            original_load = manager._load_directory_module
                            previous_workers = set(plugins_loader._ABANDONED_LOADERS)

                            def delayed_load(*values: Any, **options: Any) -> ModuleType:
                                if not release.wait(timeout=5):
                                    raise TimeoutError("fixture release deadline")
                                return original_load(*values, **options)

                            with patch.object(manager, "_load_directory_module", delayed_load):
                                try:
                                    manager.discover_and_load(force=True)
                                finally:
                                    release.set()
                                    workers = [worker for worker in plugins_loader._ABANDONED_LOADERS
                                               if worker not in previous_workers]
                                    for worker in workers:
                                        worker.join(timeout=5)
                                        self.assertFalse(worker.is_alive())
                            self.assertEqual(len(workers), 1)
                        self.assertEqual(manager._hooks, {})
                        self.assertEqual((self.root / "ready.json").read_bytes(), receipt)
                        def call() -> str:
                            return CellAuthority("reload-task").dispatch("terminal", {"command": "unchecked"})
                        if steps:
                            with self.assertRaisesRegex(SystemExit, "not loaded and enabled"):
                                call()
                            self.assertEqual(executed, [])
                        else:
                            call()
                            self.assertEqual(len(executed), 1)
                        # A coexisting native veto skips execution middleware entirely.
                        manager._hooks["pre_tool_call"] = [
                            lambda **_values: {"action": "block", "message": "existing veto"},
                        ]
                        if steps:
                            with self.assertRaisesRegex(SystemExit, "not loaded and enabled"):
                                call()
                            self.assertEqual(executed, [])
                        else:
                            self.assertIn("existing veto", call())
                            self.assertEqual(len(executed), 1)

                    with (patch.object(native, "main", dispatch),
                          patch.object(native, "_prepare_agent_startup", lambda _args: manager.discover_and_load()),
                          patch("hermes_cli.plugins.discover_plugins", manager.discover_and_load),
                          patch("hermes_cli.plugins.get_plugin_manager", return_value=manager),
                          patch.object(registry, "dispatch", handler)):
                        guard.main()

    def test_reload_between_veto_and_post_cannot_hide_audit(self) -> None:
        from agent.inline_tool_executors import emit_terminal_post_tool_call
        from hermes_cli import main as native, plugins
        from hermes_logging import _reset_queued_handlers
        from tools.code_kernel import CellAuthority
        from tools.registry import registry
        self.addCleanup(_reset_queued_handlers)
        self.addCleanup(setattr, sys, "argv", sys.argv[:])
        bundled = self.root / "bundled"
        bundled.mkdir()
        os.environ["HERMES_BUNDLED_PLUGINS"] = str(bundled)
        shutil.copytree(args.plugin, self.home / "plugins/aw-native-hooks")
        spec = importlib.util.spec_from_file_location("aw_post_guard", args.plugin / "launch.py")
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        original = plugins._dispatch_pre_tool_call_hooks
        for steps in ([("tool.before", "deny"), ("tool.after", "audit")],
                      [("tool.after", "audit")], []):
            for path in ("registry", "inline"):
                with self.subTest(steps=steps, path=path):
                    (self.root / "ready.json").unlink(missing_ok=True)
                    self.trace.unlink(missing_ok=True)
                    config = self.home / "config.yaml"
                    config.write_text('plugins: {enabled: [aw-native-hooks]}\n')
                    self.launch(steps)
                    manager = self.manager()
                    self.addCleanup(manager.unload)
                    reached = []

                    def reduction(*values: Any, **options: Any) -> Any:
                        result = original(*values, **options)
                        # Teardown starts after the guarded reducer has returned.
                        config.write_text('plugins: {enabled: []}\n')
                        worker = threading.Thread(target=manager.discover_and_load,
                                                  kwargs={"force": True})
                        worker.start()
                        worker.join(timeout=5)
                        self.assertFalse(worker.is_alive())
                        return result

                    def call() -> str:
                        if path == "registry":
                            return CellAuthority("post-task").dispatch("terminal", {"command": "blocked"})
                        message, _ = plugins._dispatch_pre_tool_call_hooks("terminal", {})
                        emit_terminal_post_tool_call(SimpleNamespace(session_id="session"),
                            function_name="terminal", function_args={}, result=message,
                            effective_task_id="task", tool_call_id="call", status="blocked")
                        return message

                    def dispatch() -> None:
                        native._prepare_agent_startup(SimpleNamespace())
                        manager._hooks.setdefault("pre_tool_call", []).append(
                            lambda **_values: {"action": "block", "message": "existing veto"})
                        if steps:
                            with self.assertRaisesRegex(SystemExit, "not loaded and enabled"):
                                call()
                        else:
                            self.assertIn("existing veto", call())
                        self.assertEqual(reached, [])

                    sys.argv = [str(args.plugin / "launch.py"), "hermes", "chat", "--cli"]
                    with (patch.object(native, "main", dispatch),
                          patch.object(native, "_prepare_agent_startup", lambda _args: manager.discover_and_load()),
                          patch("hermes_cli.plugins.discover_plugins", manager.discover_and_load),
                          patch("hermes_cli.plugins.get_plugin_manager", return_value=manager),
                          patch.object(plugins, "_dispatch_pre_tool_call_hooks", reduction),
                          patch.object(registry, "dispatch", lambda *a, **kw: reached.append("tool"))):
                        guard.main()

    def test_parallel_reload_cannot_skip_before_reduction(self) -> None:
        from hermes_cli import main as native, plugins
        from hermes_logging import _reset_queued_handlers
        self.addCleanup(_reset_queued_handlers)
        self.addCleanup(setattr, sys, "argv", sys.argv[:])
        bundled = self.root / "bundled"
        bundled.mkdir()
        os.environ["HERMES_BUNDLED_PLUGINS"] = str(bundled)
        shutil.copytree(args.plugin, self.home / "plugins/aw-native-hooks")
        config = self.home / "config.yaml"
        config.write_text('plugins: {enabled: [aw-native-hooks]}\n')
        self.launch([("tool.before", "deny"), ("tool.after", "audit")])
        manager = self.manager()
        self.addCleanup(manager.unload)
        spec = importlib.util.spec_from_file_location("aw_parallel_guard", args.plugin / "launch.py")
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        original = plugins._get_pre_tool_call_directive_details
        errors = []

        def reducer(*values: Any, **options: Any) -> Any:
            # Reload after the launcher's ledger check but before native Hook selection.
            config.write_text('plugins: {enabled: []}\n')
            def reload() -> None:
                try:
                    manager.discover_and_load(force=True)
                except RuntimeError as error:
                    errors.append(str(error))
            worker = threading.Thread(target=reload)
            worker.start()
            worker.join(timeout=5)
            self.assertFalse(worker.is_alive())
            return original(*values, **options)

        def dispatch() -> None:
            native._prepare_agent_startup(SimpleNamespace())
            message, changed = plugins._dispatch_pre_tool_call_hooks(
                "terminal", {"command": "unchecked"}, session_id="session",
                tool_call_id="call", api_request_id="request")
            self.assertEqual(message, "native block")
            self.assertIsNone(changed)
            self.assertEqual(errors, ["Hermes plugin reload during AW tool execution is unsupported"])
            # The rejected reload retained AW; between calls native reload still works.
            config.write_text('plugins: {enabled: [aw-native-hooks]}\n')
            manager.discover_and_load(force=True)
            manager.invoke_hook("post_tool_call", tool_name="terminal", args={},
                                session_id="session", tool_call_id="call", result="blocked",
                                status="blocked")
            rows = [json.loads(line) for line in self.trace.read_text().splitlines()]
            self.assertEqual([row["step"] for row in rows], ["deny", "audit"])

        sys.argv = [str(args.plugin / "launch.py"), "hermes", "chat", "--cli"]
        with (patch.object(native, "main", dispatch),
              patch.object(native, "_prepare_agent_startup", lambda _args: manager.discover_and_load()),
              patch("hermes_cli.plugins.discover_plugins", manager.discover_and_load),
              patch("hermes_cli.plugins.get_plugin_manager", return_value=manager),
              patch.object(plugins, "_get_pre_tool_call_directive_details", reducer)):
            guard.main()

    def test_in_handler_reload_cannot_remove_pending_audit(self) -> None:
        from hermes_cli import main as native, middleware
        from hermes_logging import _reset_queued_handlers
        from tools.code_kernel import CellAuthority
        from tools.registry import registry
        self.addCleanup(_reset_queued_handlers)
        self.addCleanup(setattr, sys, "argv", sys.argv[:])
        bundled = self.root / "bundled"
        bundled.mkdir()
        os.environ["HERMES_BUNDLED_PLUGINS"] = str(bundled)
        shutil.copytree(args.plugin, self.home / "plugins/aw-native-hooks")
        spec = importlib.util.spec_from_file_location("aw_in_handler_guard", args.plugin / "launch.py")
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        for steps in ([("tool.before", "observe"), ("tool.after", "audit")],
                      [("tool.after", "audit")], []):
            for phase in ("handler", "middleware"):
                with self.subTest(steps=steps, phase=phase):
                    (self.root / "ready.json").unlink(missing_ok=True)
                    self.trace.unlink(missing_ok=True)
                    config = self.home / "config.yaml"
                    config.write_text('plugins: {enabled: [aw-native-hooks]}\n')
                    self.launch(steps)
                    manager = self.manager()
                    self.addCleanup(manager.unload)
                    original_discovery = manager.discover_and_load
                    sys.argv = [str(args.plugin / "launch.py"), "hermes", "chat", "--cli"]

                    def reload() -> None:
                        config.write_text('plugins: {enabled: []}\n')
                        manager.discover_and_load(force=True)

                    def handler(_name: str, _values: dict[str, Any], **_options: Any) -> str:
                        if phase == "handler":
                            reload()
                        return '{"result":"executed"}'

                    def execution(args: dict[str, Any], next_call: Callable[..., Any],
                                  **_options: Any) -> Any:
                        result = next_call(args)
                        reload()
                        return result

                    def dispatch() -> None:
                        native._prepare_agent_startup(SimpleNamespace())
                        if phase == "middleware":
                            manager._middleware["tool_execution"] = [execution]
                        result = CellAuthority("reload-task").dispatch("terminal", {"command": "approved"})
                        rows = [json.loads(line) for line in self.trace.read_text().splitlines()] if self.trace.exists() else []
                        audit = [row for row in rows if row["step"] == "audit"]
                        if steps:
                            self.assertEqual(len(audit), 1)
                            self.assertEqual(audit[0]["payload"]["extra"]["result"], result)
                            self.assertEqual(audit[0]["payload"]["extra"]["status"],
                                             "error" if phase == "handler" else "ok")
                            self.assertTrue(manager.has_hook("post_tool_call"))
                            if phase == "handler":
                                self.assertIn("reload during AW tool execution is unsupported", result)
                        else:
                            self.assertEqual(manager._hooks, {})
                            self.assertEqual(audit, [])
                        # Reload after a settled tool still uses the native lifecycle.
                        config.write_text('plugins: {enabled: [aw-native-hooks]}\n')
                        manager.discover_and_load(force=True)
                        self.assertEqual(manager.has_hook("post_tool_call"), bool(steps))

                    with (patch.object(native, "main", dispatch),
                          patch.object(native, "_prepare_agent_startup", lambda _args: manager.discover_and_load()),
                          patch("hermes_cli.plugins.discover_plugins", manager.discover_and_load),
                          patch("hermes_cli.plugins.get_plugin_manager", return_value=manager),
                          patch.object(registry, "dispatch", handler)):
                        guard.main()
                    self.assertEqual(manager.discover_and_load, original_discovery)

    def test_post_preserves_blocked_status_and_serialized_result(self) -> None:
        self.launch([("tool.after", "observe")])
        manager = self.manager()
        self.plugin.register(self.context(manager))
        self.assertEqual(manager.invoke_hook(
            "post_tool_call", tool_name="terminal", args={}, session_id="session",
            tool_call_id="call", result='{"error":"blocked"}', status="blocked",
        ), [])
        payload = json.loads(self.trace.read_text())["payload"]
        self.assertEqual(payload["extra"]["status"], "blocked")
        self.assertEqual(payload["extra"]["result"], '{"error":"blocked"}')

    def test_official_terminal_post_preserves_all_outcomes(self) -> None:
        from agent.inline_tool_executors import emit_terminal_post_tool_call
        self.launch([("tool.after", "observe")])
        manager = self.manager()
        self.plugin.register(self.context(manager))
        agent = SimpleNamespace(session_id="session", _current_turn_id="turn",
                                _current_api_request_id="request")
        with (patch("hermes_cli.lifecycle.has_hook", manager.has_hook),
              patch("hermes_cli.lifecycle.invoke_hook", manager.invoke_hook)):
            for status in ("error", "blocked", "cancelled", "timeout"):
                emit_terminal_post_tool_call(
                    agent, function_name="terminal", function_args={"command": "fixture"},
                    result=json.dumps({"error": status}), effective_task_id="task",
                    tool_call_id="call-" + status, status=status,
                )
        rows = [json.loads(line) for line in self.trace.read_text().splitlines()]
        self.assertEqual([row["payload"]["extra"]["status"] for row in rows],
                         ["error", "blocked", "cancelled", "timeout"])
        self.assertTrue(all(row["payload"]["extra"]["api_request_id"] == "request"
                            for row in rows))

    def test_native_ask_stays_a_human_request(self) -> None:
        self.launch([("tool.before", "ask")])
        manager = self.manager()
        self.plugin.register(self.context(manager))
        self.assertEqual(manager.invoke_hook("pre_tool_call", tool_name="terminal", args={}),
                         [{"action": "approve", "message": "native human approval"}])

    def test_discovery_preserves_profile_and_relative_existing_hook(self) -> None:
        work = self.root / "work"
        work.mkdir()
        previous = Path.cwd()
        os.chdir(work)
        self.addCleanup(os.chdir, previous)
        shutil.copyfile(self.command, work / "relative-hook")
        (work / "relative-hook").chmod(0o700)
        empty_bundled = self.root / "bundled"
        empty_bundled.mkdir()
        os.environ["HERMES_BUNDLED_PLUGINS"] = str(empty_bundled)
        plugins = self.home / "plugins"
        plugins.mkdir()
        shutil.copytree(args.plugin, plugins / "aw-native-hooks")
        configuration = {
            "plugins": {"enabled": ["aw-native-hooks"]},
            "hooks": {"pre_tool_call": [{
                "command": "./relative-hook hook --step existing", "timeout": 3,
            }]},
            "custom_profile_field": {"unknown": "keep"},
        }
        original = json.dumps(configuration)
        (self.home / "config.yaml").write_text(original)
        (self.home / "auth.json").write_text("fixture authorization marker")
        (self.home / "state.db").write_bytes(b"fixture history marker")
        self.launch([("tool.before", "aw")])
        from hermes_cli.plugins import get_plugin_manager
        from agent.shell_hooks import register_from_config
        manager = get_plugin_manager()
        manager.discover_and_load()
        loaded = [p for p in manager.list_plugins() if p["name"] == "aw-native-hooks"]
        self.assertEqual(len(loaded), 1)
        self.assertTrue(loaded[0]["enabled"])
        self.assertIsNone(loaded[0]["error"])
        register_from_config(configuration, accept_hooks=True)
        manager.invoke_hook("pre_tool_call", tool_name="terminal", args={},
                            session_id="session", tool_call_id="call")
        rows = [json.loads(line) for line in self.trace.read_text().splitlines()]
        self.assertEqual([row["step"] for row in rows], ["aw", "existing"])
        self.assertEqual({row["cwd"] for row in rows}, {str(work)})
        self.assertEqual((self.home / "config.yaml").read_text(), original)
        self.assertEqual((self.home / "auth.json").read_text(), "fixture authorization marker")
        self.assertEqual((self.home / "state.db").read_bytes(), b"fixture history marker")

    def test_empty_policy_registers_no_callbacks_and_reports_readiness(self) -> None:
        self.launch([])
        manager = self.manager()
        self.plugin.register(self.context(manager))
        self.assertEqual(manager._hooks, {})
        ready = json.loads((self.root / "ready.json").read_text())
        self.assertEqual(ready["hooks"], 0)
        self.assertEqual(ready["pid"], os.getpid())
        self.assertEqual(ready["token"], "fixture-token")
        self.plugin.register(self.context(manager))
        self.assertEqual(manager._hooks, {})
        self.assertEqual(json.loads((self.root / "ready.json").read_text()), ready)

    def test_native_forced_reload_preserves_block_and_audit_callbacks(self) -> None:
        from agent.shell_hooks import register_from_config
        from hermes_cli.plugins import discover_plugins, get_plugin_manager
        from tools.voice_mode import _check_plugin_stt_provider
        empty_bundled = self.root / "bundled"
        empty_bundled.mkdir()
        os.environ["HERMES_BUNDLED_PLUGINS"] = str(empty_bundled)
        shutil.copytree(args.plugin, self.home / "plugins/aw-native-hooks")
        configuration = {"plugins": {"enabled": ["aw-native-hooks"]}, "hooks": {
            event: [{"command": shlex.join([str(self.command), "hook", "--step", step]),
                     "timeout": 3}]
            for event, step in (("pre_tool_call", "existing-before"),
                                ("post_tool_call", "existing-after"))}}
        original = json.dumps(configuration)
        (self.home / "config.yaml").write_text(original)
        launch = self.launch([("tool.before", "deny"), ("tool.after", "audit")])
        value = json.loads(launch.read_text())
        value["hooks"][0]["on_error"] = "block"
        launch.write_text(json.dumps(value))
        manager = get_plugin_manager()
        self.addCleanup(manager.unload)
        discover_plugins()
        register_from_config(configuration, accept_hooks=True)
        ready = self.root / "ready.json"
        receipt, inode = ready.read_bytes(), ready.stat().st_ino
        for attempt in range(3):
            if attempt == 1:
                from hermes_cli.env_loader import load_hermes_dotenv
                (self.home / ".env").write_text("AW_HERMES_LAUNCH=/missing/stale-launch.json\n")
                load_hermes_dotenv(hermes_home=self.home)
                self.assertEqual(os.environ["AW_HERMES_LAUNCH"], "/missing/stale-launch.json")
                self.assertFalse(_check_plugin_stt_provider("missing-aw-reload-fixture"))
            elif attempt == 2:
                os.environ.pop("AW_HERMES_LAUNCH", None)
                discover_plugins(force=True)
            loaded = [p for p in manager.list_plugins() if p["name"] == "aw-native-hooks"]
            self.assertEqual(len(loaded), 1)
            self.assertTrue(loaded[0]["enabled"], loaded[0]["error"])
            self.assertIsNone(loaded[0]["error"])
            self.assertEqual({key: len(hooks) for key, hooks in manager._hooks.items()},
                             {"pre_tool_call": 2, "post_tool_call": 2})
            self.assertEqual(ready.read_bytes(), receipt)
            self.assertEqual(ready.stat().st_ino, inode)
            call = "call-" + str(attempt)
            self.assertEqual(manager.invoke_hook(
                "pre_tool_call", tool_name="terminal", args={}, session_id="session",
                tool_call_id=call), [{"action": "block", "message": "native block"}])
            self.assertEqual(manager.invoke_hook(
                "post_tool_call", tool_name="terminal", args={}, session_id="session",
                tool_call_id=call, result='{"error":"blocked"}', status="blocked"), [])
            rows = [json.loads(line) for line in self.trace.read_text().splitlines()]
            self.assertEqual([row["step"] for row in rows],
                             ["deny", "existing-before", "audit", "existing-after"] * (attempt + 1))
            audit = rows[-2]["payload"]
            self.assertEqual(audit["extra"]["tool_call_id"], call)
            self.assertEqual(audit["extra"]["status"], "blocked")
            self.assertEqual(audit["extra"]["result"], '{"error":"blocked"}')
        self.assertEqual((self.home / "config.yaml").read_text(), original)
        self.assertEqual(list(self.root.glob("tmp*")), [])

    def test_existing_receipt_must_match_the_registration_and_be_private(self) -> None:
        from hermes_cli.plugins_manifest import parse_manifest_file
        plugin = self.home / "plugins/aw-native-hooks"
        shutil.copytree(args.plugin, plugin)
        manifest = parse_manifest_file(plugin / "plugin.yaml", plugin, "user", "")
        self.launch([("tool.before", "deny")])
        manager = self.manager()
        self.addCleanup(manager.unload)
        receipt = {"version": 1, "adapter": "hermes", "token": "fixture-token",
                   "pid": os.getpid(), "hooks": 1}
        ready = self.root / "ready.json"
        for key, value in (("token", "other-session"), ("pid", os.getpid() + 1), ("hooks", 2)):
            with self.subTest(field=key):
                original = json.dumps({**receipt, key: value})
                ready.write_text(original)
                ready.chmod(0o600)
                manager._load_plugin(manifest)
                self.assertFalse(manager.list_plugins()[0]["enabled"])
                self.assertIn("another registration", manager.list_plugins()[0]["error"])
                self.assertEqual(manager._hooks, {})
                self.assertEqual(ready.read_text(), original)
        ready.write_text(json.dumps(receipt))
        ready.chmod(0o644)
        manager._load_plugin(manifest)
        self.assertIn("unsafe", manager.list_plugins()[0]["error"])
        self.assertEqual(manager._hooks, {})
        saved = self.root / "saved-receipt"
        ready.rename(saved)
        saved.chmod(0o600)
        ready.symlink_to(saved)
        manager._load_plugin(manifest)
        self.assertFalse(manager.list_plugins()[0]["enabled"])
        self.assertEqual(manager._hooks, {})
        self.assertTrue(ready.is_symlink())
        self.assertEqual(json.loads(saved.read_text()), receipt)
        self.assertEqual(list(self.root.glob("tmp*")), [])

    def test_regular_hermes_launch_is_inert(self) -> None:
        os.environ["AW_HERMES_LAUNCH"] = "/missing/stale-launch.json"
        manager = self.manager()
        self.plugin.register(self.context(manager))
        self.assertEqual(manager._hooks, {})

    def test_cli_flag_overrides_tui_preferences_on_tty(self) -> None:
        from hermes_cli.main_tui_launch import _resolve_use_tui
        with (patch("sys.stdin.isatty", return_value=True),
              patch("sys.stdout.isatty", return_value=True),
              patch("hermes_cli.config.load_config", return_value={"display": {"interface": "tui"}})):
            for preference in ("0", "1"):
                with patch.dict(os.environ, {"HERMES_TUI": preference}):
                    self.assertTrue(_resolve_use_tui(SimpleNamespace(cli=False)))
                    self.assertFalse(_resolve_use_tui(SimpleNamespace(cli=True)))

    def test_profile_switch_and_unsafe_file_are_rejected(self) -> None:
        path = self.launch([("tool.before", "observe")])
        manager = self.manager()
        path.chmod(0o644)
        with self.assertRaisesRegex(ValueError, "unsafe"):
            self.plugin.register(self.context(manager))
        path.chmod(0o600)
        value = json.loads(path.read_text())
        value["profile"] = str(self.root)
        path.write_text(json.dumps(value))
        with self.assertRaisesRegex(ValueError, "profile changed"):
            self.plugin.register(self.context(manager))
        self.assertEqual(manager._hooks, {})


if __name__ == "__main__":
    sys.path.insert(0, str(args.source))
    unittest.main(argv=[sys.argv[0]])
