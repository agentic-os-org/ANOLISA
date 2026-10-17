"""Hermes native reload contracts."""

from __future__ import annotations
import importlib.util
import json
import os
import shlex
import shutil
import sys
import threading
from unittest.mock import patch
from types import ModuleType, SimpleNamespace
from typing import Any, Callable


from native_support import NativeFixture


class ReloadTests(NativeFixture):
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
        shutil.copytree(self.plugin_path, self.home / "plugins/aw-native-hooks")
        spec = importlib.util.spec_from_file_location(
            "aw_reload_guard", self.plugin_path / "launch.py"
        )
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        for steps in (
            [("tool.before", "deny"), ("tool.after", "audit")],
            [("tool.after", "audit")],
            [],
        ):
            for failure in ("omitted", "timeout"):
                with self.subTest(steps=steps, failure=failure):
                    (self.root / "ready.json").unlink(missing_ok=True)
                    self.trace.unlink(missing_ok=True)
                    config = self.home / "config.yaml"
                    config.write_text("plugins: {enabled: [aw-native-hooks]}\n")
                    self.launch(steps)
                    manager = self.manager()
                    self.addCleanup(manager.unload)
                    sys.argv = [str(self.plugin_path / "launch.py"), "hermes", "chat", "--cli"]
                    executed = []

                    def handler(_name: str, values: dict[str, Any], **_options: Any) -> str:
                        executed.append(values)
                        return '{"result":"executed"}'

                    def dispatch() -> None:
                        native._prepare_agent_startup(SimpleNamespace())
                        receipt = (self.root / "ready.json").read_bytes()
                        if failure == "omitted":
                            config.write_text("plugins: {enabled: []}\n")
                            manager.discover_and_load(force=True)
                        else:
                            config.write_text(
                                "plugins: {enabled: [aw-native-hooks], load_timeout_seconds: 0.001}\n"
                            )
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
                                    workers = [
                                        worker
                                        for worker in plugins_loader._ABANDONED_LOADERS
                                        if worker not in previous_workers
                                    ]
                                    for worker in workers:
                                        worker.join(timeout=5)
                                        self.assertFalse(worker.is_alive())
                            self.assertEqual(len(workers), 1)
                        self.assertEqual(manager._hooks, {})
                        self.assertEqual((self.root / "ready.json").read_bytes(), receipt)

                        def call() -> str:
                            return CellAuthority("reload-task").dispatch(
                                "terminal", {"command": "unchecked"}
                            )

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

                    with (
                        patch.object(native, "main", dispatch),
                        patch.object(
                            native,
                            "_prepare_agent_startup",
                            lambda _args: manager.discover_and_load(),
                        ),
                        patch("hermes_cli.plugins.discover_plugins", manager.discover_and_load),
                        patch("hermes_cli.plugins.get_plugin_manager", return_value=manager),
                        patch.object(registry, "dispatch", handler),
                    ):
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
        shutil.copytree(self.plugin_path, self.home / "plugins/aw-native-hooks")
        spec = importlib.util.spec_from_file_location(
            "aw_post_guard", self.plugin_path / "launch.py"
        )
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        original = plugins._dispatch_pre_tool_call_hooks
        for steps in (
            [("tool.before", "deny"), ("tool.after", "audit")],
            [("tool.after", "audit")],
            [],
        ):
            for path in ("registry", "inline"):
                with self.subTest(steps=steps, path=path):
                    (self.root / "ready.json").unlink(missing_ok=True)
                    self.trace.unlink(missing_ok=True)
                    config = self.home / "config.yaml"
                    config.write_text("plugins: {enabled: [aw-native-hooks]}\n")
                    self.launch(steps)
                    manager = self.manager()
                    self.addCleanup(manager.unload)
                    reached = []

                    def reduction(*values: Any, **options: Any) -> Any:
                        result = original(*values, **options)
                        # Teardown starts after the guarded reducer has returned.
                        config.write_text("plugins: {enabled: []}\n")
                        worker = threading.Thread(
                            target=manager.discover_and_load, kwargs={"force": True}
                        )
                        worker.start()
                        worker.join(timeout=5)
                        self.assertFalse(worker.is_alive())
                        return result

                    def call() -> str:
                        if path == "registry":
                            return CellAuthority("post-task").dispatch(
                                "terminal", {"command": "blocked"}
                            )
                        message, _ = plugins._dispatch_pre_tool_call_hooks("terminal", {})
                        emit_terminal_post_tool_call(
                            SimpleNamespace(session_id="session"),
                            function_name="terminal",
                            function_args={},
                            result=message,
                            effective_task_id="task",
                            tool_call_id="call",
                            status="blocked",
                        )
                        return message

                    def dispatch() -> None:
                        native._prepare_agent_startup(SimpleNamespace())
                        manager._hooks.setdefault("pre_tool_call", []).append(
                            lambda **_values: {"action": "block", "message": "existing veto"}
                        )
                        if steps:
                            with self.assertRaisesRegex(SystemExit, "not loaded and enabled"):
                                call()
                        else:
                            self.assertIn("existing veto", call())
                        self.assertEqual(reached, [])

                    sys.argv = [str(self.plugin_path / "launch.py"), "hermes", "chat", "--cli"]
                    with (
                        patch.object(native, "main", dispatch),
                        patch.object(
                            native,
                            "_prepare_agent_startup",
                            lambda _args: manager.discover_and_load(),
                        ),
                        patch("hermes_cli.plugins.discover_plugins", manager.discover_and_load),
                        patch("hermes_cli.plugins.get_plugin_manager", return_value=manager),
                        patch.object(plugins, "_dispatch_pre_tool_call_hooks", reduction),
                        patch.object(registry, "dispatch", lambda *a, **kw: reached.append("tool")),
                    ):
                        guard.main()

    def test_parallel_reload_cannot_skip_before_reduction(self) -> None:
        from hermes_cli import main as native, plugins
        from hermes_logging import _reset_queued_handlers

        self.addCleanup(_reset_queued_handlers)
        self.addCleanup(setattr, sys, "argv", sys.argv[:])
        bundled = self.root / "bundled"
        bundled.mkdir()
        os.environ["HERMES_BUNDLED_PLUGINS"] = str(bundled)
        shutil.copytree(self.plugin_path, self.home / "plugins/aw-native-hooks")
        config = self.home / "config.yaml"
        config.write_text("plugins: {enabled: [aw-native-hooks]}\n")
        self.launch([("tool.before", "deny"), ("tool.after", "audit")])
        manager = self.manager()
        self.addCleanup(manager.unload)
        spec = importlib.util.spec_from_file_location(
            "aw_parallel_guard", self.plugin_path / "launch.py"
        )
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        original = plugins._get_pre_tool_call_directive_details
        errors = []

        def reducer(*values: Any, **options: Any) -> Any:
            # Reload after the launcher's ledger check but before native Hook selection.
            config.write_text("plugins: {enabled: []}\n")

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
                "terminal",
                {"command": "unchecked"},
                session_id="session",
                tool_call_id="call",
                api_request_id="request",
            )
            self.assertEqual(message, "native block")
            self.assertIsNone(changed)
            self.assertEqual(
                errors, ["Hermes plugin reload during AW tool execution is unsupported"]
            )
            # The rejected reload retained AW; between calls native reload still works.
            config.write_text("plugins: {enabled: [aw-native-hooks]}\n")
            manager.discover_and_load(force=True)
            manager.invoke_hook(
                "post_tool_call",
                tool_name="terminal",
                args={},
                session_id="session",
                tool_call_id="call",
                result="blocked",
                status="blocked",
            )
            rows = [json.loads(line) for line in self.trace.read_text().splitlines()]
            self.assertEqual([row["step"] for row in rows], ["deny", "audit"])

        sys.argv = [str(self.plugin_path / "launch.py"), "hermes", "chat", "--cli"]
        with (
            patch.object(native, "main", dispatch),
            patch.object(
                native, "_prepare_agent_startup", lambda _args: manager.discover_and_load()
            ),
            patch("hermes_cli.plugins.discover_plugins", manager.discover_and_load),
            patch("hermes_cli.plugins.get_plugin_manager", return_value=manager),
            patch.object(plugins, "_get_pre_tool_call_directive_details", reducer),
        ):
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
        shutil.copytree(self.plugin_path, self.home / "plugins/aw-native-hooks")
        spec = importlib.util.spec_from_file_location(
            "aw_in_handler_guard", self.plugin_path / "launch.py"
        )
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        for steps in (
            [("tool.before", "observe"), ("tool.after", "audit")],
            [("tool.after", "audit")],
            [],
        ):
            for phase in ("handler", "middleware"):
                with self.subTest(steps=steps, phase=phase):
                    (self.root / "ready.json").unlink(missing_ok=True)
                    self.trace.unlink(missing_ok=True)
                    config = self.home / "config.yaml"
                    config.write_text("plugins: {enabled: [aw-native-hooks]}\n")
                    self.launch(steps)
                    manager = self.manager()
                    self.addCleanup(manager.unload)
                    original_discovery = manager.discover_and_load
                    sys.argv = [str(self.plugin_path / "launch.py"), "hermes", "chat", "--cli"]

                    def reload() -> None:
                        config.write_text("plugins: {enabled: []}\n")
                        manager.discover_and_load(force=True)

                    def handler(_name: str, _values: dict[str, Any], **_options: Any) -> str:
                        if phase == "handler":
                            reload()
                        return '{"result":"executed"}'

                    def execution(
                        args: dict[str, Any], next_call: Callable[..., Any], **_options: Any
                    ) -> Any:
                        result = next_call(args)
                        reload()
                        return result

                    def dispatch() -> None:
                        native._prepare_agent_startup(SimpleNamespace())
                        if phase == "middleware":
                            manager._middleware["tool_execution"] = [execution]
                        result = CellAuthority("reload-task").dispatch(
                            "terminal", {"command": "approved"}
                        )
                        rows = (
                            [json.loads(line) for line in self.trace.read_text().splitlines()]
                            if self.trace.exists()
                            else []
                        )
                        audit = [row for row in rows if row["step"] == "audit"]
                        if steps:
                            self.assertEqual(len(audit), 1)
                            self.assertEqual(audit[0]["payload"]["extra"]["result"], result)
                            self.assertEqual(
                                audit[0]["payload"]["extra"]["status"],
                                "error" if phase == "handler" else "ok",
                            )
                            self.assertTrue(manager.has_hook("post_tool_call"))
                            if phase == "handler":
                                self.assertIn(
                                    "reload during AW tool execution is unsupported", result
                                )
                        else:
                            self.assertEqual(manager._hooks, {})
                            self.assertEqual(audit, [])
                        # Reload after a settled tool still uses the native lifecycle.
                        config.write_text("plugins: {enabled: [aw-native-hooks]}\n")
                        manager.discover_and_load(force=True)
                        self.assertEqual(manager.has_hook("post_tool_call"), bool(steps))

                    with (
                        patch.object(native, "main", dispatch),
                        patch.object(
                            native,
                            "_prepare_agent_startup",
                            lambda _args: manager.discover_and_load(),
                        ),
                        patch("hermes_cli.plugins.discover_plugins", manager.discover_and_load),
                        patch("hermes_cli.plugins.get_plugin_manager", return_value=manager),
                        patch.object(registry, "dispatch", handler),
                    ):
                        guard.main()
                    self.assertEqual(manager.discover_and_load, original_discovery)

    def test_native_forced_reload_preserves_block_and_audit_callbacks(self) -> None:
        from agent.shell_hooks import register_from_config
        from hermes_cli.plugins import discover_plugins, get_plugin_manager
        from tools.voice_mode import _check_plugin_stt_provider

        empty_bundled = self.root / "bundled"
        empty_bundled.mkdir()
        os.environ["HERMES_BUNDLED_PLUGINS"] = str(empty_bundled)
        shutil.copytree(self.plugin_path, self.home / "plugins/aw-native-hooks")
        configuration = {
            "plugins": {"enabled": ["aw-native-hooks"]},
            "hooks": {
                event: [
                    {
                        "command": shlex.join([str(self.command), "hook", "--step", step]),
                        "timeout": 3,
                    }
                ]
                for event, step in (
                    ("pre_tool_call", "existing-before"),
                    ("post_tool_call", "existing-after"),
                )
            },
        }
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
            self.assertEqual(
                {key: len(hooks) for key, hooks in manager._hooks.items()},
                {"pre_tool_call": 2, "post_tool_call": 2},
            )
            self.assertEqual(ready.read_bytes(), receipt)
            self.assertEqual(ready.stat().st_ino, inode)
            call = "call-" + str(attempt)
            self.assertEqual(
                manager.invoke_hook(
                    "pre_tool_call",
                    tool_name="terminal",
                    args={},
                    session_id="session",
                    tool_call_id=call,
                ),
                [{"action": "block", "message": "native block"}],
            )
            self.assertEqual(
                manager.invoke_hook(
                    "post_tool_call",
                    tool_name="terminal",
                    args={},
                    session_id="session",
                    tool_call_id=call,
                    result='{"error":"blocked"}',
                    status="blocked",
                ),
                [],
            )
            rows = [json.loads(line) for line in self.trace.read_text().splitlines()]
            self.assertEqual(
                [row["step"] for row in rows],
                ["deny", "existing-before", "audit", "existing-after"] * (attempt + 1),
            )
            audit = rows[-2]["payload"]
            self.assertEqual(audit["extra"]["tool_call_id"], call)
            self.assertEqual(audit["extra"]["status"], "blocked")
            self.assertEqual(audit["extra"]["result"], '{"error":"blocked"}')
        self.assertEqual((self.home / "config.yaml").read_text(), original)
        self.assertEqual(list(self.root.glob("tmp*")), [])
