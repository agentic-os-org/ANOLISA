"""Hermes native hooks contracts."""

from __future__ import annotations
import json
import os
from pathlib import Path
import shutil
from unittest.mock import patch
from types import SimpleNamespace


from native_support import NativeFixture


class HooksTests(NativeFixture):
    def test_replacement_uses_the_final_native_transform_seam(self) -> None:
        launch = self.launch(
            [("tool.after", "observe"), ("tool.after", "user"), ("tool.after", "compress")]
        )
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

        with (
            patch("hermes_cli.lifecycle.has_hook", lambda event: bool(manager._hooks.get(event))),
            patch("hermes_cli.lifecycle.invoke_hook", manager.invoke_hook),
            patch.object(self.plugin.subprocess, "run", checked_run),
        ):
            model_tools._emit_post_tool_call_hook(
                function_name="terminal",
                function_args={"command": "echo text"},
                result='{"output":"original","exit_code":0}',
                duration_ms=1,
                task_id="task",
                session_id="session",
                tool_call_id="call",
                turn_id="turn",
                api_request_id="request",
            )
            result = model_tools._apply_transform_tool_result_hook(
                "terminal", {"command": "echo text"}, '{"output":"original","exit_code":0}', 1, ids
            )
        self.assertEqual(result, "compact")
        rows = [json.loads(row) for row in self.trace.read_text().splitlines()]
        self.assertEqual([row["step"] for row in rows], ["observe", "user", "compress"])
        self.assertTrue(
            all(
                row["payload"]["extra"]["result"] == '{"output":"original","exit_code":0}'
                for row in rows
            )
        )
        payload = rows[-1]["payload"]
        self.assertEqual(payload["hook_event_name"], "transform_tool_result")
        self.assertEqual(payload["extra"]["api_request_id"], "request")

    def test_native_order_and_block_keep_later_callbacks(self) -> None:
        self.launch([("tool.before", "deny"), ("tool.before", "after-deny")])
        manager = self.manager()
        self.plugin.register(self.context(manager))
        results = manager.invoke_hook(
            "pre_tool_call",
            tool_name="arbitrary_tool",
            args={"unicode": "猫", "nested": [1, False]},
            session_id="session",
            tool_call_id="call",
            custom={"field": 4},
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
        self.assertEqual(
            manager.invoke_hook(
                "post_tool_call",
                tool_name="terminal",
                args={},
                session_id="session",
                tool_call_id="call",
                result='{"error":"blocked"}',
                status="blocked",
            ),
            [],
        )
        payload = json.loads(self.trace.read_text())["payload"]
        self.assertEqual(payload["extra"]["status"], "blocked")
        self.assertEqual(payload["extra"]["result"], '{"error":"blocked"}')

    def test_official_terminal_post_preserves_all_outcomes(self) -> None:
        from agent.inline_tool_executors import emit_terminal_post_tool_call

        self.launch([("tool.after", "observe")])
        manager = self.manager()
        self.plugin.register(self.context(manager))
        agent = SimpleNamespace(
            session_id="session", _current_turn_id="turn", _current_api_request_id="request"
        )
        with (
            patch("hermes_cli.lifecycle.has_hook", manager.has_hook),
            patch("hermes_cli.lifecycle.invoke_hook", manager.invoke_hook),
        ):
            for status in ("error", "blocked", "cancelled", "timeout"):
                emit_terminal_post_tool_call(
                    agent,
                    function_name="terminal",
                    function_args={"command": "fixture"},
                    result=json.dumps({"error": status}),
                    effective_task_id="task",
                    tool_call_id="call-" + status,
                    status=status,
                )
        rows = [json.loads(line) for line in self.trace.read_text().splitlines()]
        self.assertEqual(
            [row["payload"]["extra"]["status"] for row in rows],
            ["error", "blocked", "cancelled", "timeout"],
        )
        self.assertTrue(all(row["payload"]["extra"]["api_request_id"] == "request" for row in rows))

    def test_native_ask_stays_a_human_request(self) -> None:
        self.launch([("tool.before", "ask")])
        manager = self.manager()
        self.plugin.register(self.context(manager))
        self.assertEqual(
            manager.invoke_hook("pre_tool_call", tool_name="terminal", args={}),
            [{"action": "approve", "message": "native human approval"}],
        )

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
        shutil.copytree(self.plugin_path, plugins / "aw-native-hooks")
        configuration = {
            "plugins": {"enabled": ["aw-native-hooks"]},
            "hooks": {
                "pre_tool_call": [
                    {
                        "command": "./relative-hook hook --step existing",
                        "timeout": 3,
                    }
                ]
            },
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
        manager.invoke_hook(
            "pre_tool_call",
            tool_name="terminal",
            args={},
            session_id="session",
            tool_call_id="call",
        )
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

    def test_existing_receipt_must_match_the_registration_and_be_private(self) -> None:
        from hermes_cli.plugins_manifest import parse_manifest_file

        plugin = self.home / "plugins/aw-native-hooks"
        shutil.copytree(self.plugin_path, plugin)
        manifest = parse_manifest_file(plugin / "plugin.yaml", plugin, "user", "")
        self.launch([("tool.before", "deny")])
        manager = self.manager()
        self.addCleanup(manager.unload)
        receipt = {
            "version": 1,
            "adapter": "hermes",
            "token": "fixture-token",
            "pid": os.getpid(),
            "hooks": 1,
        }
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

        with (
            patch("sys.stdin.isatty", return_value=True),
            patch("sys.stdout.isatty", return_value=True),
            patch("hermes_cli.config.load_config", return_value={"display": {"interface": "tui"}}),
        ):
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
