"""Exercise the adapter with the pinned Hermes dispatcher and an owned profile."""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace

sys.dont_write_bytecode = True


parser = argparse.ArgumentParser()
parser.add_argument("--source", type=Path, required=True)
parser.add_argument("--plugin", type=Path, required=True)
args = parser.parse_args()


class NativeContract(unittest.TestCase):
    def setUp(self) -> None:
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
        return launch

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

    def test_regular_hermes_launch_is_inert(self) -> None:
        os.environ.pop("AW_HERMES_LAUNCH", None)
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
