"""Pinned Hermes source resolution and pre-render snapshot adoption."""
import importlib.util
import json
import os
from pathlib import Path
from concurrent.futures import ThreadPoolExecutor
from unittest.mock import patch

from native_support import NativeFixture


class SkillTests(NativeFixture):
    def setUp(self):
        super().setUp()
        spec = importlib.util.spec_from_file_location("aw_hermes_skill_test", self.plugin_path / "aw_skill.py")
        self.skills = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.skills)
        from tools import skills_tool
        self.native = skills_tool
        self.addCleanup(self.skills.install())
        self.folder = self.home / "skills"
        self.folder.mkdir()

    def source(self, name, body="PRIVATE_INSTRUCTIONS", directory=None):
        path = self.folder / (directory or name)
        path.mkdir(parents=True)
        (path / "SKILL.md").write_text(f"---\nname: {name}\ndescription: fixture\n---\n{body}\n")
        return path

    def execute(self, name, before=None, denied=False, arguments=None):
        arguments = arguments or {"name": name}
        rows = []
        def check(**kwargs):
            rows.append(kwargs)
            if before:
                before()
            return {"action": "block", "message": "denied"} if denied else None
        hook = self.skills.callback(check, {"skill_check": True, "on_error": "block"})
        with self.skills.invocation("skill_view", arguments):
            result = hook(tool_name="skill_view", args=arguments)
            if result and result.get("action") == "block":
                return rows, result
            return rows, json.loads(self.native.skill_view(**arguments))

    def test_native_frontmatter_selection_and_checked_text(self):
        source = self.source("alias", directory="category/directory")
        rows, result = self.execute("alias")
        self.assertEqual(rows[0]["aw_skill"], {"name": "alias", "directory": str(source), "source": "hermes-local"})
        self.assertIn("PRIVATE_INSTRUCTIONS", json.dumps(result))
        self.assertIsNone(self.skills._selection.get())

    def test_denial_precedes_native_rendering_and_inline_shell(self):
        self.source("denied")
        with patch.object(self.native, "_preprocess_skill", side_effect=AssertionError("unchecked render")):
            _, result = self.execute("denied", denied=True)
        self.assertEqual(result["action"], "block")
        self.assertNotIn("PRIVATE_INSTRUCTIONS", json.dumps(result))

    def test_opened_file_must_match_the_resolved_manifest(self):
        self.source("local")
        other = self.root / "untrusted"
        other.write_text("UNCHECKED_BODY")
        original = os.open
        with patch.object(self.skills.os, "open", lambda *_args: original(other, os.O_RDONLY)):
            rows, result = self.execute("local")
        self.assertEqual(rows[0]["aw_skill"], {"error_code": "skill_resolution_error"})
        self.assertEqual(result["action"], "block")

    def test_source_edit_blocks_before_rendering(self):
        source = self.source("changed")
        _, result = self.execute("changed", before=lambda: (source / "SKILL.md").write_text("UNTRUSTED"))
        self.assertEqual(result["action"], "block")

    def test_new_competitor_after_check_cannot_replace_selection(self):
        self.source("same", directory="one")
        rows, result = self.execute("same", before=lambda: self.source("same", "UNCHECKED", directory="two"))
        self.assertIn("directory", rows[0]["aw_skill"])
        self.assertFalse(result["success"])
        self.assertNotIn("PRIVATE_INSTRUCTIONS", json.dumps(result))
        self.assertNotIn("UNCHECKED", json.dumps(result))

    def test_missing_ambiguous_plugin_flat_and_reference_sources_fail_resolution(self):
        self.source("same", directory="one")
        self.source("same", directory="two")
        self.source("local")
        (self.folder / "flat.md").write_text("legacy")
        for name, args in [("same", None), ("missing", None), ("plugin:skill", None), ("flat", None),
                           ("local", {"name": "local", "file_path": "references/a.md"})]:
            rows, result = self.execute(name, arguments=args)
            self.assertEqual(rows[0]["aw_skill"], {"error_code": "skill_resolution_error"})
            self.assertEqual(result["action"], "block")

    def test_unchecked_native_delivery_is_rejected_and_context_is_released(self):
        self.source("local")
        with self.skills.invocation("skill_view", {"name": "local"}):
            result = json.loads(self.native.skill_view("local"))
            self.assertNotIn("PRIVATE_INSTRUCTIONS", json.dumps(result))
        self.assertIsNone(self.skills._selection.get())
        self.assertIn("PRIVATE_INSTRUCTIONS", self.native.skill_view("local"))

    def test_policy_denial_survives_source_edit(self):
        source = self.source("denied")
        _, result = self.execute("denied", denied=True, before=lambda: (source / "SKILL.md").unlink())
        self.assertEqual(result, {"action": "block", "message": "denied"})

    def test_concurrent_invocations_keep_distinct_sources(self):
        for name in ["one", "two"]:
            self.source(name, name + "_BODY")
        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(self.execute, ["one", "two"]))
        for name, (rows, result) in zip(["one", "two"], results):
            self.assertEqual(rows[0]["aw_skill"]["name"], name)
            self.assertIn(name + "_BODY", json.dumps(result))

    def test_all_pretool_steps_share_source_metadata(self):
        self.source("local")
        rows = []
        def collect(**kwargs):
            rows.append(kwargs)
        with self.skills.invocation("skill_view", {"name": "local"}):
            for checking in (False, True, False):
                callback = self.skills.callback(collect, {"skill_check": checking, "on_error": "block"})
                callback(tool_name="skill_view", args={"name": "local"})
            self.assertEqual(rows[0], rows[1])
            self.assertEqual(rows[1], rows[2])
            self.assertIn("PRIVATE_INSTRUCTIONS", self.native.skill_view("local"))

    def test_overridden_skill_handler_cannot_reuse_a_checked_builtin_asset(self):
        from copy import copy
        from tools.registry import registry
        self.source("local")
        entry = copy(registry.get_entry("skill_view"))
        entry.handler = lambda *_args, **_kwargs: "UNCHECKED_REPLACEMENT"
        with patch.dict(registry._tools, {"skill_view": entry}):
            rows, result = self.execute("local")
            self.assertEqual(rows[0]["aw_skill"], {"error_code": "skill_resolution_error"})
            self.assertEqual(result["action"], "block")
        args = {"name": "local"}
        with self.skills.invocation("skill_view", args):
            check = self.skills.callback(lambda **_kwargs: None, {"skill_check": True, "on_error": "block"})
            self.assertIsNone(check(tool_name="skill_view", args=args))
            with patch.dict(registry._tools, {"skill_view": entry}):
                with self.assertRaisesRegex(OSError, "handler changed"):
                    registry.dispatch("skill_view", args)

    def test_report_preserves_native_behavior_for_unresolved_sources(self):
        (self.folder / "flat.md").write_text("LEGACY_BODY")
        args = {"name": "flat"}
        check = self.skills.callback(lambda **_kwargs: None,
                                     {"skill_check": True, "on_error": "report"})
        with self.skills.invocation("skill_view", args):
            self.assertIsNone(check(tool_name="skill_view", args=args))
            self.assertIn("LEGACY_BODY", self.native.skill_view("flat"))

    def test_native_disabled_skill_remains_denied(self):
        self.source("disabled")
        with patch.object(self.native, "_is_skill_disabled", return_value=True):
            _, result = self.execute("disabled")
        self.assertFalse(result["success"])
        self.assertNotIn("PRIVATE_INSTRUCTIONS", json.dumps(result))

    def test_launch_dispatch_adopts_the_checked_native_source(self):
        import shutil
        import sys
        from types import SimpleNamespace
        from hermes_cli import main as native
        from hermes_logging import _reset_queued_handlers
        import model_tools

        self.addCleanup(_reset_queued_handlers)
        self.source("local")
        launch = self.launch([("tool.before", "check")])
        config = json.loads(launch.read_text())
        config["hooks"][0].update(skill_check=True, on_error="block")
        launch.write_text(json.dumps(config))
        plugin_path = self.home / "plugins/aw-native-hooks"
        shutil.copytree(self.plugin_path, plugin_path)
        (self.home / "config.yaml").write_text("plugins: {enabled: [aw-native-hooks]}\n")
        manager = self.manager()
        self.addCleanup(manager.unload)
        bundled = self.root / "bundled"
        bundled.mkdir()
        os.environ["HERMES_BUNDLED_PLUGINS"] = str(bundled)
        spec = importlib.util.spec_from_file_location("aw_hermes_skill_launch", self.plugin_path / "launch.py")
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        results = []

        def run():
            native._prepare_agent_startup(SimpleNamespace())
            for denied in [False, True]:
                if denied:
                    config["hooks"][0]["step"] = "deny"
                    launch.write_text(json.dumps(config))
                    manager.discover_and_load(force=True)
                results.append(model_tools.handle_function_call("skill_view", {"name": "local"},
                    task_id="task", session_id="session", tool_call_id="call-" + str(denied), api_request_id="request"))

        with (patch.dict(sys.modules, {"aw_skill": self.skills}),
              patch.object(sys, "argv", [str(self.plugin_path / "launch.py"), "hermes", "chat", "--cli"]),
              patch.object(native, "main", run),
              patch.object(native, "_prepare_agent_startup", lambda _args: manager.discover_and_load()),
              patch("hermes_cli.plugins.discover_plugins", manager.discover_and_load),
              patch("hermes_cli.plugins.get_plugin_manager", return_value=manager)):
            guard.main()
        self.assertIn("PRIVATE_INSTRUCTIONS", results[0])
        self.assertNotIn("PRIVATE_INSTRUCTIONS", results[1])
        rows = [json.loads(row) for row in self.trace.read_text().splitlines()]
        self.assertEqual(rows[0]["payload"]["extra"]["aw_skill"]["directory"], str(self.folder / "local"))
        self.assertIsNone(self.skills._selection.get())

    def dynamic_execute(self, name, mode, before=None, output=None):
        from agent.shell_hooks import ShellHookSpec
        arguments = {"name": name}
        captured = []
        def spawn(_spec, data):
            captured.append(json.loads(data))
            if before:
                before()
            return {"returncode": 0, "stdout": json.dumps(output if output is not None else {"aw_skill_check": mode}),
                    "stderr": "", "timed_out": False, "error": None, "elapsed_seconds": 0}
        spec = ShellHookSpec(event="pre_tool_call", command="fixture", timeout=3, fail_closed=True)
        with self.skills.invocation("skill_view", arguments, dynamic=True), patch("agent.shell_hooks._spawn", spawn):
            reply = self.skills.dynamic_callback(spec)(tool_name="skill_view", args=arguments)
            if reply and reply.get("action") == "block":
                return captured, reply
            return captured, json.loads(self.native.skill_view(**arguments))

    def test_dynamic_callback_retains_checked_input_attestation(self):
        from contextvars import ContextVar
        from types import SimpleNamespace
        from agent.shell_hooks import ShellHookSpec
        cell = {}
        session = SimpleNamespace(rewritten_input=ContextVar("test_checked", default=cell))
        spec = ShellHookSpec(event="pre_tool_call", command="fixture", timeout=3, fail_closed=True)
        revised = {"command":"printf checked", "timeout":7}
        response = {"returncode":0, "stdout":json.dumps({"action":"modify", "args":revised,
            "aw_checked_input":True, "aw_skill_check":None}), "stderr":"", "timed_out":False,
            "error":None, "elapsed_seconds":0}
        with patch("agent.shell_hooks._spawn", return_value=response):
            reply = self.skills.dynamic_callback(spec, lambda r, n: self.plugin._record_input(r, n, session))(
                tool_name="terminal", args={"command":"original", "timeout":7})
        self.assertEqual(reply["args"], revised)
        self.assertEqual(json.loads(cell["input"]), revised)

    def test_dynamic_skill_policy_can_enable_then_remove_source_pinning(self):
        path = self.source("local")
        rows, reply = self.dynamic_execute("local", "block")
        self.assertEqual(rows[0]["extra"]["aw_skill"]["directory"], str(path))
        self.assertIn("PRIVATE_INSTRUCTIONS", json.dumps(reply))
        rows, reply = self.dynamic_execute("local", "block",
            before=lambda: (path / "SKILL.md").write_text("CHANGED_INSTRUCTIONS"))
        self.assertEqual(reply["action"], "block")
        self.assertNotIn("CHANGED_INSTRUCTIONS", json.dumps(reply))
        (path / "SKILL.md").write_text("NATIVE_BEHAVIOR")
        _, reply = self.dynamic_execute("local", None,
            before=lambda: (path / "SKILL.md").write_text("CURRENT_NATIVE_BODY"))
        self.assertIn("CURRENT_NATIVE_BODY", json.dumps(reply))
        self.assertIsNone(self.skills._selection.get())

    def test_dynamic_skill_resolution_preserves_report_and_rejects_missing_admission(self):
        (self.folder / "flat.md").write_text("LEGACY_BODY")
        _, blocked = self.dynamic_execute("flat", "block")
        self.assertEqual(blocked["action"], "block")
        _, report = self.dynamic_execute("flat", "report")
        self.assertIn("LEGACY_BODY", json.dumps(report))
        _, absent = self.dynamic_execute("flat", None, output={})
        self.assertEqual(absent["action"], "block")
        self.assertNotIn("LEGACY_BODY", json.dumps(absent))
