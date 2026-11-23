"""Exercise submission transport against real subprocess callbacks."""

import importlib.util
import json
from pathlib import Path
from unittest.mock import patch

from native_support import NativeFixture


class SubmissionTests(NativeFixture):
    def setUp(self):
        super().setUp()
        spec = importlib.util.spec_from_file_location("aw_hermes_input", self.plugin_path / "launch.py")
        self.guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.guard)
        self.config = json.loads(self.launch([]).read_text())

    def check(self, steps, text="hello"):
        self.config["submissions"] = [
            {"step": step, "on_error": action, "timeout": 3} for step, action in steps
        ]
        return self.guard.check_submission(self.config, "session", text)

    def rows(self):
        return [json.loads(line) for line in self.trace.read_text().splitlines()]

    def test_allow_shares_identity_then_closes(self):
        self.assertIsNone(self.check([("one", "report"), ("two", "block")]))
        rows = self.rows()
        self.assertEqual([row["step"] for row in rows], ["one", "two", "one"])
        self.assertEqual(len({row["payload"]["aw_turn_id"] for row in rows}), 1)
        self.assertTrue(all(row["payload"]["input_text"] == "hello" for row in rows))

    def test_denial_skips_later_steps_and_next_input_gets_new_identity(self):
        self.assertIn("blocked", self.check([("deny", "report"), ("later", "report")]))
        self.assertIsNone(self.check([("allow", "block")], "next"))
        rows = self.rows()
        self.assertEqual([row["step"] for row in rows], ["deny", "deny", "allow", "allow"])
        self.assertNotEqual(rows[0]["payload"]["aw_turn_id"], rows[2]["payload"]["aw_turn_id"])

    def test_callback_error_follows_action_and_closes(self):
        self.assertIn("blocked", self.check([("error", "block")]))
        self.assertIsNone(self.check([("error", "report"), ("later", "block")]))
        self.assertEqual([row["step"] for row in self.rows()], ["error", "error", "error", "later", "error"])

    def test_invalid_media_and_encoded_size_never_dispatch(self):
        for value in ([{"type": "image_url"}], "", "\ud800", "x" * 262144):
            with self.subTest(value=type(value).__name__):
                self.assertIsNotNone(self.check([("allow", "report")], value))
        self.assertFalse(self.trace.exists())

    def test_cleanup_failure_never_allows_a_native_turn(self):
        original = self.guard.subprocess.run

        def fail_close(argv, **options):
            if "--close-event" in argv:
                raise OSError("fixture cleanup failed")
            return original(argv, **options)

        with patch.object(self.guard.subprocess, "run", fail_close):
            with self.assertRaisesRegex(OSError, "cleanup failed"):
                self.check([("allow", "report")])

    def test_launcher_checks_before_image_routing_and_restores_native_entry(self):
        import shutil
        import sys
        from types import SimpleNamespace
        from hermes_cli import main as native
        from hermes_cli.plugins_manifest import parse_manifest_file
        from hermes_logging import _reset_queued_handlers
        import cli

        self.addCleanup(_reset_queued_handlers)
        self.addCleanup(setattr, sys, "argv", sys.argv[:])
        path = self.launch([])
        config = json.loads(path.read_text())
        config["submissions"] = [{"step": "deny", "on_error": "block", "timeout": 3}]
        path.write_text(json.dumps(config))
        plugin_path = self.home / "plugins/aw-native-hooks"
        shutil.copytree(self.plugin_path, plugin_path)
        manifest = parse_manifest_file(plugin_path / "plugin.yaml", plugin_path, "user", "")
        manager = self.manager()
        original_main = cli.main
        original_images = cli._route_single_query_images
        dispatched = []

        def dispatch():
            native._prepare_agent_startup(SimpleNamespace())
            with self.assertRaisesRegex(SystemExit, "quiet single-query"):
                cli.main(query="", quiet=True)
            with self.assertRaisesRegex(SystemExit, "image input"):
                cli._route_single_query_images(SimpleNamespace(session_id="session"), "image", "image", ["a.png"], [])
            with self.assertRaises(SystemExit) as denied:
                cli._route_single_query_images(SimpleNamespace(session_id="session"), "text", "text", [], [])
            self.assertEqual(denied.exception.code, 2)

        sys.argv = [str(self.plugin_path / "launch.py"), "hermes", "chat", "--cli"]
        with (
            patch.object(native, "main", dispatch),
            patch.object(native, "_prepare_agent_startup", lambda _args: manager._load_plugin(manifest)),
            patch("hermes_cli.plugins.discover_plugins", lambda: None),
            patch("hermes_cli.plugins.get_plugin_manager", return_value=manager),
            patch.object(cli, "_route_single_query_images", lambda *_args: dispatched.append(True)),
        ):
            saved_images = cli._route_single_query_images
            self.guard.main()
            self.assertIs(cli.main, original_main)
            self.assertIs(cli._route_single_query_images, saved_images)
        self.assertIs(cli._route_single_query_images, original_images)
        self.assertEqual(dispatched, [])
        self.assertEqual([row["step"] for row in self.rows()], ["deny", "deny"])
