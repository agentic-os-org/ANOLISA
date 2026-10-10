"""Exercise the adapter with the pinned Hermes dispatcher and an owned profile."""

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
        def dispatch():
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
        def dispatch():
            native._prepare_agent_startup(SimpleNamespace())
            reached.append("chat")
        original_import = __import__
        def load_profile_environment(name, *values, **options):
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
        def dispatch():
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
        def delayed_load(*values, **options):
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
        def dispatch():
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

    def manager(self):
        from hermes_cli.plugins import PluginManager
        return PluginManager(scope_key=str(self.home))

    def context(self, manager):
        class Context:
            def register_hook(self, name, callback):
                manager._hooks.setdefault(name, []).append(callback)
        return Context()

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
