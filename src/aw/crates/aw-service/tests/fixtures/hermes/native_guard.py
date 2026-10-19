"""Hermes native guard contracts."""

from __future__ import annotations
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import sys
import py_compile
import struct
import threading
from unittest.mock import patch
from types import ModuleType, SimpleNamespace
from typing import Any, Callable


from native_support import NativeFixture


class GuardTests(NativeFixture):
    def test_launch_guard_requires_receipt_before_chat_dispatch(self) -> None:
        self.launch([])
        guard_path = self.plugin_path / "launch.py"
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
        with (
            patch.object(native, "main", dispatch),
            patch.object(native, "_prepare_agent_startup", lambda _args: None),
            patch("hermes_cli.plugins.discover_plugins", lambda: None),
        ):
            with self.assertRaisesRegex(SystemExit, "not registered before chat"):
                guard.main()
        self.assertEqual(reached, [])
        self.assertNotIn("_aw_hermes_session", sys.modules)

    def test_callback_timeout_uses_native_yaml_and_environment_semantics(self) -> None:
        from hermes_cli.plugins import _resolve_hook_callback_timeout

        os.environ["AW_HERMES_TIMEOUT"] = "80"
        for scalar, expected in (
            ("1:20", 80),
            ("012", 10),
            ('"${AW_HERMES_TIMEOUT}"', 80),
            ("0", 0),
            ("-1", 30),
            ("1000", 600),
        ):
            (self.home / "config.yaml").write_text(
                "plugins:\n  hook_callback_timeout: " + scalar + "\n"
            )
            self.assertEqual(_resolve_hook_callback_timeout(), expected, scalar)
        os.environ.pop("AW_HERMES_TIMEOUT")

    def test_guard_keeps_launcher_binding_when_native_import_overwrites_environment(self) -> None:
        from hermes_cli import main as native
        from hermes_cli.plugins_manifest import parse_manifest_file
        from hermes_logging import _reset_queued_handlers

        self.addCleanup(_reset_queued_handlers)
        plugin_path = self.home / "plugins/aw-native-hooks"
        shutil.copytree(self.plugin_path, plugin_path)
        launch = self.launch([("tool.before", "observe")])
        stale = self.root / "stale-launch.json"
        value = json.loads(launch.read_text())
        value.update(token="stale-token", hooks=[], ready_path=str(self.root / "stale-ready.json"))
        stale.write_text(json.dumps(value))
        stale.chmod(0o600)
        manager = self.manager()
        manifest = parse_manifest_file(plugin_path / "plugin.yaml", plugin_path, "user", "")
        guard_path = self.plugin_path / "launch.py"
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
        with (
            patch.object(native, "main", dispatch),
            patch.object(
                native, "_prepare_agent_startup", lambda _args: manager._load_plugin(manifest)
            ),
            patch("hermes_cli.plugins.discover_plugins", lambda: None),
            patch("hermes_cli.plugins.get_plugin_manager", return_value=manager),
            patch("builtins.__import__", load_profile_environment),
        ):
            guard.main()
        self.assertEqual(os.environ["AW_HERMES_LAUNCH"], str(launch))
        self.assertEqual(reached, ["chat"])
        self.assertNotIn("_aw_hermes_session", sys.modules)
        self.assertTrue(manager.has_hook("pre_tool_call"))
        self.assertEqual(
            json.loads((self.root / "ready.json").read_text())["token"], "fixture-token"
        )
        self.assertFalse((self.root / "stale-ready.json").exists())

    def test_guard_ignores_forged_profile_bytecode(self) -> None:
        from hermes_cli import main as native
        from hermes_cli.plugins_manifest import parse_manifest_file
        from hermes_logging import _reset_queued_handlers

        self.addCleanup(_reset_queued_handlers)
        plugin_path = self.home / "plugins/aw-native-hooks"
        shutil.copytree(self.plugin_path, plugin_path)
        init = plugin_path / "__init__.py"
        cache = Path(importlib.util.cache_from_source(str(init)))
        cache.parent.mkdir(mode=0o775)
        cache.parent.chmod(0o775)
        marker = self.root / "forged-cache-executed"
        poison = self.root / "poison.py"
        poison.write_text("from pathlib import Path\nPath(" + repr(str(marker)) + ").touch()\n")
        py_compile.compile(
            str(poison),
            cfile=str(cache),
            doraise=True,
            invalidation_mode=py_compile.PycInvalidationMode.TIMESTAMP,
        )
        content = cache.read_bytes()
        info = init.stat()
        cache.write_bytes(
            content[:8] + struct.pack("<II", int(info.st_mtime), info.st_size) + content[16:]
        )
        cache.chmod(0o664)
        manifest = parse_manifest_file(plugin_path / "plugin.yaml", plugin_path, "user", "")
        # Control: the official loader executes timestamp-valid cached code.
        self.manager()._load_directory_module(manifest)
        self.assertTrue(marker.exists())
        marker.unlink()
        self.launch([("tool.before", "observe")])
        manager = self.manager()
        guard_path = self.plugin_path / "launch.py"
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
        with (
            patch.object(native, "main", dispatch),
            patch.object(
                native, "_prepare_agent_startup", lambda _args: manager._load_plugin(manifest)
            ),
            patch("hermes_cli.plugins.discover_plugins", lambda: None),
            patch("hermes_cli.plugins.get_plugin_manager", return_value=manager),
        ):
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
        shutil.copytree(self.plugin_path, plugin_path)
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
                workers = [
                    worker
                    for worker in plugins_loader._ABANDONED_LOADERS
                    if worker not in previous_workers
                ]
                for worker in workers:
                    worker.join(timeout=5)
                    self.assertFalse(worker.is_alive())
        self.assertEqual(len(workers), 1)
        self.assertFalse(manager.list_plugins()[0]["enabled"])
        self.assertEqual(manager._hooks, {})
        ready = json.loads((self.root / "ready.json").read_text())
        self.assertEqual(ready["hooks"], 1)
        guard_path = self.plugin_path / "launch.py"
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
        with (
            patch.object(native, "main", dispatch),
            patch.object(native, "_prepare_agent_startup", lambda _args: None),
            patch("hermes_cli.plugins.discover_plugins", lambda: None),
            patch("hermes_cli.plugins.get_plugin_manager", return_value=manager),
        ):
            with self.assertRaisesRegex(SystemExit, "not loaded and enabled"):
                guard.main()
        self.assertEqual(reached, [])

    def test_guard_rejects_native_rewrites_only_with_before_callbacks(self) -> None:
        from agent.shell_hooks import ShellHookSpec, _make_callback
        from hermes_cli import main as native, plugins
        from hermes_logging import _reset_queued_handlers

        self.addCleanup(_reset_queued_handlers)
        self.addCleanup(setattr, sys, "argv", sys.argv[:])
        plugin_path = self.home / "plugins/aw-native-hooks"
        shutil.copytree(self.plugin_path, plugin_path)
        (self.home / "config.yaml").write_text("plugins: {enabled: [aw-native-hooks]}\n")
        bundled = self.root / "bundled"
        bundled.mkdir()
        os.environ["HERMES_BUNDLED_PLUGINS"] = str(bundled)
        spec = importlib.util.spec_from_file_location(
            "aw_hermes_rewrite_guard", self.plugin_path / "launch.py"
        )
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        original_reducer = plugins._get_pre_tool_call_directive_details
        modifier = _make_callback(
            ShellHookSpec(
                event="pre_tool_call",
                timeout=3,
                command=shlex.join([str(self.command), "hook", "--step", "modify"]),
            )
        )
        for steps in (
            [("tool.before", "observe"), ("tool.after", "audit")],
            [("tool.before", "deny"), ("tool.after", "audit")],
            [("tool.after", "audit")],
            [],
        ):
            with self.subTest(steps=steps):
                (self.root / "ready.json").unlink(missing_ok=True)
                self.trace.unlink(missing_ok=True)
                self.launch(steps)
                manager = self.manager()
                self.addCleanup(manager.unload)
                sys.argv = [str(self.plugin_path / "launch.py"), "hermes", "chat", "--cli"]

                def dispatch() -> None:
                    native._prepare_agent_startup(SimpleNamespace())
                    for reloading in (False, True):
                        if reloading:
                            manager.discover_and_load(force=True)
                        for first in (True, False):
                            hooks = [
                                hook
                                for hook in manager._hooks.get("pre_tool_call", [])
                                if hook is not modifier
                            ]
                            hooks.insert(0 if first else len(hooks), modifier)
                            manager._hooks["pre_tool_call"] = hooks
                            original = {"command": "printf approved", "retained": "猫"}
                            message, changed = plugins._dispatch_pre_tool_call_hooks(
                                "terminal",
                                original,
                                session_id="session",
                                tool_call_id="call",
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
                                self.assertEqual(
                                    changed,
                                    {**original, "command": "printf rewritten > rewritten.txt"},
                                )
                            self.assertEqual(original["command"], "printf approved")
                            manager.invoke_hook(
                                "post_tool_call",
                                tool_name="terminal",
                                args=changed or original,
                                session_id="session",
                                tool_call_id="call",
                                result='{"error":"blocked"}' if message else "ok",
                                status="blocked" if message else "success",
                            )

                with (
                    patch.object(native, "main", dispatch),
                    patch.object(
                        native, "_prepare_agent_startup", lambda _args: manager.discover_and_load()
                    ),
                    patch("hermes_cli.plugins.discover_plugins", manager.discover_and_load),
                    patch("hermes_cli.plugins.get_plugin_manager", return_value=manager),
                ):
                    guard.main()
                self.assertIs(plugins._get_pre_tool_call_directive_details, original_reducer)
                rows = [json.loads(line) for line in self.trace.read_text().splitlines()]
                before = [row for row in rows if row["step"] in ("observe", "deny")]
                audit = [row for row in rows if row["step"] == "audit"]
                self.assertEqual(len(before), 4 if steps and steps[0][0] == "tool.before" else 0)
                self.assertEqual(len(audit), 4 if steps else 0)
                if before:
                    self.assertTrue(
                        all(
                            row["payload"]["tool_input"]["command"] == "printf approved"
                            for row in before
                        )
                    )
                    self.assertTrue(
                        all(row["payload"]["extra"]["status"] == "blocked" for row in audit)
                    )
                self.assertFalse((Path.cwd() / "rewritten.txt").exists())

    def test_execution_middleware_cannot_rewrite_nested_code_calls(self) -> None:
        from hermes_cli import main as native, middleware, plugins
        from hermes_logging import _reset_queued_handlers
        from tools.code_kernel import CellAuthority
        from tools.registry import registry

        self.addCleanup(_reset_queued_handlers)
        self.addCleanup(setattr, sys, "argv", sys.argv[:])
        plugin_path = self.home / "plugins/aw-native-hooks"
        shutil.copytree(self.plugin_path, plugin_path)
        (self.home / "config.yaml").write_text("plugins: {enabled: [aw-native-hooks]}\n")
        bundled = self.root / "bundled"
        bundled.mkdir()
        os.environ["HERMES_BUNDLED_PLUGINS"] = str(bundled)
        spec = importlib.util.spec_from_file_location(
            "aw_hermes_execution_guard", self.plugin_path / "launch.py"
        )
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        original_execution = middleware.run_tool_execution_middleware
        for steps in (
            [("tool.before", "observe"), ("tool.after", "audit")],
            [("tool.after", "audit")],
            [],
        ):
            with self.subTest(steps=steps):
                (self.root / "ready.json").unlink(missing_ok=True)
                self.trace.unlink(missing_ok=True)
                self.launch(steps)
                manager = self.manager()
                self.addCleanup(manager.unload)
                sys.argv = [str(self.plugin_path / "launch.py"), "hermes", "chat", "--cli"]
                executed = []

                def handler(_name: str, values: dict[str, Any], **_options: Any) -> str:
                    executed.append(json.loads(json.dumps(values)))
                    return '{"result":"executed"}'

                def dispatch() -> None:
                    native._prepare_agent_startup(SimpleNamespace())
                    for reloading in (False, True):
                        if reloading:
                            manager.discover_and_load(force=True)
                        for mode in (
                            "replacement",
                            "in-place",
                            "type",
                            "tuple",
                            "integer-key",
                            "unsigned-overflow",
                            "signed-overflow",
                            "nan",
                            "infinite",
                            "surrogate",
                            "surrogate-key",
                            "integer-bounds",
                            "observe",
                        ):

                            def execution(
                                args: dict[str, Any], next_call: Callable[..., Any], **_options: Any
                            ) -> Any:
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
                                "nan": [float("nan")],
                                "infinite": [float("inf")],
                                "surrogate": ["\ud800"],
                                "surrogate-key": [{"\ud800": "value"}],
                                "integer-bounds": [-(1 << 63), (1 << 64) - 1],
                            }.get(mode, [1])
                            result = CellAuthority("code-task").dispatch(
                                "terminal",
                                {
                                    "command": "approved",
                                    "nested": {
                                        "allowed": True,
                                        "items": items,
                                        "mapping": {"1": "approved"},
                                    },
                                    "flag": True,
                                },
                            )
                            blocked = bool(
                                steps
                                and steps[0][0] == "tool.before"
                                and mode not in ("observe", "integer-bounds")
                            )
                            if blocked:
                                self.assertEqual(executed, [])
                                error = (
                                    "modification is unsupported"
                                    if mode in ("replacement", "in-place", "type")
                                    else "unsupported JSON"
                                )
                                self.assertIn(error, json.loads(result)["error"])
                            else:
                                self.assertEqual(len(executed), 1)

                with (
                    patch.object(native, "main", dispatch),
                    patch.object(
                        native, "_prepare_agent_startup", lambda _args: manager.discover_and_load()
                    ),
                    patch("hermes_cli.plugins.discover_plugins", manager.discover_and_load),
                    patch("hermes_cli.plugins.get_plugin_manager", return_value=manager),
                    patch.object(registry, "dispatch", handler),
                ):
                    guard.main()
                self.assertIs(middleware.run_tool_execution_middleware, original_execution)
                rows = (
                    [json.loads(line) for line in self.trace.read_text().splitlines()]
                    if self.trace.exists()
                    else []
                )
                before = [row for row in rows if row["step"] == "observe"]
                audit = [row for row in rows if row["step"] == "audit"]
                self.assertEqual(len(before), 26 if steps and steps[0][0] == "tool.before" else 0)
                self.assertEqual(len(audit), 26 if steps else 0)
                self.assertTrue(
                    all(row["payload"]["tool_input"]["command"] == "approved" for row in before)
                )
                if before:
                    self.assertEqual(
                        [row["payload"]["extra"]["status"] for row in audit],
                        (["error"] * 11 + ["ok"] * 2) * 2,
                    )
