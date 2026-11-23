"""Check the pinned native chat seam and plugin registry under policy admission."""

import importlib.util
import json
import shutil
import sys
from types import SimpleNamespace
from unittest.mock import patch

from native_support import NativeFixture


class AdmissionTests(NativeFixture):
    def test_chat_seam_admits_before_dispatch_and_keeps_cancellation_busy(self) -> None:
        from hermes_cli import main as native
        from hermes_logging import _reset_queued_handlers
        import cli

        self.addCleanup(_reset_queued_handlers)
        self.addCleanup(setattr, sys, "argv", sys.argv[:])
        self.addCleanup(setattr, sys, "path", sys.path[:])
        sys.path.insert(0, str(self.plugin_path))
        shutil.copytree(self.plugin_path, self.home / "plugins/aw-native-hooks")
        (self.home / "config.yaml").write_text("plugins: {enabled: [aw-native-hooks]}\n")
        launch = self.launch([("tool.before", "$aw.dispatch"), ("tool.after", "$aw.dispatch")])
        value = json.loads(launch.read_text())
        value["reloadable"] = True
        value["hooks"][1]["replace_result"] = True
        launch.write_text(json.dumps(value))
        manager = self.manager()
        self.addCleanup(manager.unload)
        spec = importlib.util.spec_from_file_location("aw_admission_guard", self.plugin_path / "launch.py")
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        sys.argv = [str(self.plugin_path / "launch.py"), "hermes", "chat", "--cli"]
        reached = []

        def native_chat(client, message):
            phases = [row["payload"].get("phase") for row in
                      map(json.loads, self.trace.read_text().splitlines())]
            self.assertEqual(phases[-1], "request_start")
            reached.append(message)
            return "reply" if message == "complete" else None

        def dispatch():
            native._prepare_agent_startup(SimpleNamespace())
            self.assertEqual(cli.HermesCLI.chat(object(), "complete"), "reply")
            self.assertIsNone(cli.HermesCLI.chat(object(), "cancel"))
            with self.assertRaisesRegex(RuntimeError, "restart required"):
                cli.HermesCLI.chat(object(), "unconfirmed")

        with (
            patch.object(native, "main", dispatch),
            patch.object(native, "_prepare_agent_startup", lambda _args: manager.discover_and_load()),
            patch("hermes_cli.plugins.discover_plugins", manager.discover_and_load),
            patch("hermes_cli.plugins.get_plugin_manager", return_value=manager),
            patch.object(cli.HermesCLI, "chat", native_chat),
        ):
            guard.main()
        phases = [row["payload"].get("phase") for row in
                  map(json.loads, self.trace.read_text().splitlines())]
        self.assertEqual(phases, ["ready", "request_start", "request_end", "request_start"])
        self.assertEqual(reached, ["complete", "cancel"])
