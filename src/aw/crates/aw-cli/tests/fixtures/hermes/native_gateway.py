"""Check the Gateway-owned tool hook boundary on Hermes releases that provide it."""

from concurrent.futures import ThreadPoolExecutor
import asyncio
import importlib.util
import json
import shutil
from pathlib import Path
import sys
from types import SimpleNamespace
from unittest.mock import patch

from native_support import NativeFixture


class GatewayTests(NativeFixture):
    def setUp(self):
        super().setUp()
        from hermes_logging import _reset_queued_handlers
        self.addCleanup(_reset_queued_handlers)

    def test_cli_registers_gateway_before_native_turn(self):
        try:
            from hermes_cli.gateway_agent import GatewayAgent
        except ImportError:
            self.skipTest("Hermes has no Gateway Agent")
        from hermes_cli import main as native
        from hermes_cli.plugins_manifest import parse_manifest_file

        launch = self.launch([("tool.before", "observe")])
        config = json.loads(launch.read_text())
        config["revision"] = "a" * 40
        launch.write_text(json.dumps(config))
        plugin_path = self.home / "plugins" / "aw-native-hooks"
        shutil.copytree(self.plugin_path, plugin_path)
        manager = self.manager()
        manifest = parse_manifest_file(plugin_path / "plugin.yaml", plugin_path, "user", "")
        guard_path = self.plugin_path / "launch.py"
        spec = importlib.util.spec_from_file_location("aw_hermes_gateway_launch", guard_path)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        calls = []
        outcome = {"value": "native"}
        fail_prime = {"once": True}
        zero_hooks = {"once": True}
        retire_busy = {"once": True}

        def original_run(agent, query):
            calls.append(("turn", query))
            return outcome["value"]

        def rpc(method, **params):
            calls.append((method, params.get("session_id") or params.get("target_session_id")))
            if method == "plugin.aw-native-hooks.retire":
                self.assertEqual(set(params), {"target_session_id", "launch_path", "token", "timeout"})
                if retire_busy["once"]:
                    retire_busy["once"] = False
                    return {"retired": False}
                return {"retired": True}
            self.assertEqual(method, "plugin.aw-native-hooks.prime")
            self.assertEqual(set(params), {"session_id", "launch_path", "token"})
            if fail_prime["once"]:
                fail_prime["once"] = False
                raise OSError("lost registration reply")
            if zero_hooks["once"]:
                zero_hooks["once"] = False
                return {"adapter": "hermes", "token": "fixture-token", "hooks": 0,
                        "gateway_pid": 123}
            return {"adapter": "hermes", "token": "fixture-token", "hooks": 1,
                    "gateway_pid": 123}

        def dispatch():
            native._prepare_agent_startup(SimpleNamespace())
            agent = SimpleNamespace(session_id="session", connection=SimpleNamespace(call=rpc))
            with self.assertRaisesRegex(OSError, "lost registration reply"):
                GatewayAgent.run_conversation(agent, "lost registration")
            self.assertEqual(json.loads(launch.with_name("hermes-sessions.json").read_text()),
                             ["session"])
            with self.assertRaisesRegex(RuntimeError, "did not register the AW callbacks"):
                GatewayAgent.run_conversation(agent, "zero callbacks")
            self.assertEqual(GatewayAgent.run_conversation(agent, "hello"), "native")
            outcome["value"] = {"failed": True, "failure_reason": "gateway_connection"}
            self.assertEqual(GatewayAgent.run_conversation(agent, "disconnected"), outcome["value"])
            outcome["value"] = {"admitted": False, "failed": True}
            self.assertEqual(GatewayAgent.run_conversation(agent, "not sent"), outcome["value"])
            outcome["value"] = {"completed": True, "adopted": True}
            self.assertEqual(GatewayAgent.run_conversation(agent, "adopted terminal"), outcome["value"])

        self.addCleanup(setattr, sys, "argv", sys.argv[:])
        sys.argv = [str(guard_path), "hermes", "chat", "--cli"]
        with (patch.object(native, "main", dispatch),
              patch.object(native, "_prepare_agent_startup", lambda _args: manager._load_plugin(manifest)),
              patch("hermes_cli.plugins.discover_plugins", lambda: None),
              patch("hermes_cli.plugins.get_plugin_manager", return_value=manager),
              patch("hermes_cli.gateway_connection.GatewayConnection",
                    return_value=SimpleNamespace(call=rpc, close=lambda: None)),
              patch("hermes_cli.version_info.get_version_info", return_value=SimpleNamespace(commit="a" * 40)),
              patch.object(GatewayAgent, "run_conversation", original_run)):
            guard.main()
        self.assertFalse(launch.with_name("hermes-retired.json").exists())
        self.assertEqual(calls, [
            ("plugin.aw-native-hooks.prime", "session"),
            ("plugin.aw-native-hooks.prime", "session"),
            ("plugin.aw-native-hooks.prime", "session"), ("turn", "hello"),
            ("plugin.aw-native-hooks.prime", "session"), ("turn", "disconnected"),
            ("plugin.aw-native-hooks.prime", "session"), ("turn", "not sent"),
            ("plugin.aw-native-hooks.prime", "session"), ("turn", "adopted terminal"),
        ])

    def bind_gateway(self, steps, *, replace_result=False, skill_check=False,
                     turn_run=None, prime_only=False, missing_hook=None):
        try:
            from hermes_cli.gateway_agent import GatewayAgent  # noqa: F401
        except ImportError:
            self.skipTest("Hermes has no Gateway Agent")

        launch = self.launch(steps)
        config = json.loads(launch.read_text())
        config["revision"] = "a" * 40
        if skill_check:
            config["hooks"][0]["skill_check"] = True
        if replace_result:
            config["hooks"][-1]["replace_result"] = True
        launch.write_text(json.dumps(config))
        sys.modules.pop("_aw_hermes_session", None)
        spec = importlib.util.spec_from_file_location(
            "aw_hermes_gateway_fixture", self.plugin_path / "__init__.py",
            submodule_search_locations=[str(self.plugin_path)],
        )
        plugin = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = plugin
        self.addCleanup(sys.modules.pop, spec.name, None)
        spec.loader.exec_module(plugin)
        hooks, rpc, cleanups = {}, {}, []
        manager = self.manager()
        activate = turn_run is None and not prime_only
        if activate:
            async def turn_run(_authority, _ref, _live, _row):
                raise RuntimeError("fixture admission remains unknown")
        if turn_run is not None:
            from gateway.session_authority import SessionAuthority
            turn_patch = patch.object(SessionAuthority, "_execute_claimed", turn_run)
            turn_patch.start()
            self.addCleanup(turn_patch.stop)
        def register_hook(name, callback):
            hooks.setdefault(name, callback)
            if missing_hook not in (name, "*"):
                manager._hooks.setdefault(name, []).append(callback)

        ctx = SimpleNamespace(
            register_hook=register_hook,
            register_rpc=lambda name, callback: rpc.setdefault(name, callback),
            on_unload=cleanups.append,
            session_cwd=lambda: str(Path.cwd()),
            _manager=manager,
        )
        version = patch("hermes_cli.version_info.get_version_info", return_value=SimpleNamespace(commit="a" * 40))
        version.start()
        self.addCleanup(version.stop)
        plugin.register(ctx)
        self.addCleanup(cleanups[0])
        from hermes_cli import middleware

        original = middleware.run_tool_execution_middleware
        middleware.run_tool_execution_middleware = lambda name, args, next_call, **kw: next_call(args)
        self.addCleanup(setattr, middleware, "run_tool_execution_middleware", original)
        params = {"session_id": "session", "launch_path": str(launch),
                  "token": "fixture-token"}
        if missing_hook:
            with self.assertRaisesRegex(ValueError, "callbacks are not active"):
                rpc["prime"](params, session_id="session")
            return hooks, middleware
        receipt = rpc["prime"](params, session_id="session")
        if activate:
            from gateway.session_authority import SessionAuthority
            with self.assertRaisesRegex(RuntimeError, "fixture admission remains unknown"):
                asyncio.run(SessionAuthority._execute_claimed(
                    SimpleNamespace(), SimpleNamespace(session_id="session"), None,
                    {"request_id": "fixture-submission"}))
        self.assertEqual(receipt["hooks"], len(steps))
        self.gateway_rpc, self.gateway_manager, self.gateway_receipt = rpc, manager, receipt
        return hooks, middleware

    def test_before_blocks_modified_execution_and_after_replaces_result(self):
        hooks, middleware = self.bind_gateway([
            ("tool.before", "observe"), ("tool.after", "observe"), ("tool.after", "compress")
        ], replace_result=True)
        launch = Path(__import__("os").environ["AW_HERMES_LAUNCH"])
        ids = dict(session_id="session", conversation_id="session", api_request_id="request", tool_call_id="call")
        self.assertIsNone(hooks["pre_tool_call"](
            tool_name="terminal", args={"command": "echo ok"}, **ids))
        hooks["post_tool_call"](
            tool_name="terminal", args={"command": "echo ok"}, result="original", **ids)
        import model_tools
        native_ids = model_tools._CallIds(
            "task", "session", "call", "turn", "request", "session")
        with (patch("hermes_cli.lifecycle.has_hook", lambda event: event == "transform_tool_result"),
              patch("hermes_cli.lifecycle.invoke_hook",
                    lambda event, **values: [hooks[event](**values)])):
            result = model_tools._apply_transform_tool_result_hook(
                "terminal", {"command": "echo ok"}, "original", 1, native_ids)
        self.assertEqual(result, "compact")
        rows = [json.loads(line) for line in self.trace.read_text().splitlines()]
        self.assertEqual([row["step"] for row in rows], ["observe", "observe", "compress"])
        self.assertEqual(rows[-1]["payload"]["hook_event_name"], "transform_tool_result")
        self.assertEqual(rows[-1]["payload"]["extra"]["result"], "original")

        # The bound launch is immutable across Gateway callbacks.
        config = json.loads(launch.read_text())
        config["token"] = "changed"
        launch.write_text(json.dumps(config))
        denied = hooks["pre_tool_call"](tool_name="terminal", args={"command": "echo ok"}, **ids)
        self.assertEqual(denied["action"], "block")

    def test_before_checks_execution_arguments(self):
        hooks, middleware = self.bind_gateway([("tool.before", "observe")])
        self.assertIsNone(hooks["pre_tool_call"](
            tool_name="terminal", args={"command": "echo other"},
            session_id="other-session", conversation_id="session",
            api_request_id="other-request", tool_call_id="other-call"))
        ids = dict(session_id="session", api_request_id="request", tool_call_id="call")
        from hermes_cli import plugins
        with patch("hermes_cli.lifecycle.invoke_hook",
                   lambda event, **values: [hooks[event](**values)]):
            self.assertEqual(plugins._dispatch_pre_tool_call_hooks(
                "terminal", {"command": "echo ok"}, **ids), (None, None))
            ids["conversation_id"] = "different-conversation"
            self.assertEqual(plugins._dispatch_pre_tool_call_hooks(
                "terminal", {"command": "echo ok"}, **ids), (None, None))
        with patch("hermes_cli.lifecycle.invoke_hook", lambda event, **values: []):
            blocked, _ = plugins._dispatch_pre_tool_call_hooks(
                "terminal", {"command": "echo changed"}, **ids)
        self.assertEqual(blocked, "AW Hermes checked tool input changed")
        with patch("hermes_cli.lifecycle.invoke_hook",
                   lambda event, **values: [hooks[event](**values),
                                            {"action": "modify", "args": {"command": "echo changed"}}]):
            blocked, _ = plugins._dispatch_pre_tool_call_hooks(
                "terminal", {"command": "echo ok"}, **ids)
        self.assertEqual(blocked, "AW Hermes checked tool input changed")
        self.assertEqual(middleware.run_tool_execution_middleware(
            "terminal", {"command": "echo ok"}, lambda args: "executed", **ids), "executed")
        self.assertIn("AW Hermes checked tool input changed", middleware.run_tool_execution_middleware(
            "terminal", {"command": "echo changed"}, lambda args: "executed", **ids))

    def test_checked_input_replacement_is_the_only_executable_input(self):
        hooks, _ = self.bind_gateway([("tool.before", "checked-modify")])
        ids = dict(session_id="session", conversation_id="session", api_request_id="request", tool_call_id="call")
        from hermes_cli import plugins
        with patch("hermes_cli.lifecycle.invoke_hook",
                   lambda event, **values: [hooks[event](**values)]):
            blocked, changed = plugins._dispatch_pre_tool_call_hooks(
                "terminal", {"command": "echo original"}, **ids)
        self.assertIsNone(blocked)
        self.assertEqual(changed["command"], "printf checked")
        import model_tools
        self.assertIn("AW Hermes checked tool input changed", model_tools.handle_function_call(
            "terminal", {"command": "echo original"}, "task", **ids,
            skip_pre_tool_call_hook=True, skip_tool_request_middleware=True,
            skip_tool_execution_middleware=True))

    def test_before_denial_prevents_execution(self):
        hooks, _ = self.bind_gateway([("tool.before", "deny")])
        ids = dict(session_id="session", conversation_id="session", api_request_id="request", tool_call_id="call")
        from hermes_cli import plugins
        with patch("hermes_cli.lifecycle.invoke_hook",
                   lambda event, **values: [hooks[event](**values)]):
            blocked, _ = plugins._dispatch_pre_tool_call_hooks(
                "terminal", {"command": "echo no"}, **ids)
        self.assertEqual(blocked, "native block")

    def test_native_approval_request_reaches_hermes(self):
        hooks, _ = self.bind_gateway([("tool.before", "ask")])
        ids = dict(session_id="session", conversation_id="session", api_request_id="request", tool_call_id="call")
        from hermes_cli.plugins import _get_pre_tool_call_directive_details
        with patch("hermes_cli.lifecycle.invoke_hook",
                   lambda event, **values: [hooks[event](**values)]):
            details = _get_pre_tool_call_directive_details("terminal", {"command": "echo ok"}, **ids)
        self.assertEqual(details.action, "approve")

    def test_after_only_leaves_pre_tool_admission_untouched(self):
        hooks, middleware = self.bind_gateway([("tool.after", "observe")])
        ids = dict(session_id="session", conversation_id="session", api_request_id="request", tool_call_id="call")
        self.assertIsNone(hooks["pre_tool_call"](tool_name="terminal", args={}, session_id="session"))
        self.assertEqual(middleware.run_tool_execution_middleware(
            "terminal", {"command": "echo ok"}, lambda args: "executed", **ids), "executed")
        hooks["post_tool_call"](tool_name="terminal", args={}, result="executed", **ids)
        self.assertEqual(json.loads(self.trace.read_text())["step"], "observe")

    def test_gateway_skill_view_uses_checked_v0231_source(self):
        skill = self.home / "skills" / "sample"
        skill.mkdir(parents=True)
        (skill / "SKILL.md").write_text(
            "---\nname: sample\ndescription: fixture\n---\nCHECKED_SKILL_CONTENT\n")
        hooks, _ = self.bind_gateway([("tool.before", "observe")], skill_check=True)
        ids = dict(session_id="session", conversation_id="session", api_request_id="request", tool_call_id="call")
        self.assertIsNone(hooks["pre_tool_call"](
            tool_name="skill_view", args={"name": "sample"}, **ids))
        import model_tools
        result = model_tools.handle_function_call(
            "skill_view", {"name": "sample"}, "task", **ids,
            skip_pre_tool_call_hook=True, skip_tool_request_middleware=True,
            skip_tool_execution_middleware=True)
        self.assertIn("CHECKED_SKILL_CONTENT", result)
        payload = json.loads(self.trace.read_text())["payload"]
        self.assertEqual(payload["extra"]["aw_skill"]["name"], "sample")

    def test_compaction_keeps_same_turn_bound_and_rejects_unknown_lineage(self):
        hooks, middleware = self.bind_gateway([("tool.before", "observe")])
        from hermes_cli import plugins

        root = dict(session_id="session", conversation_id="session",
                    api_request_id="request-1", tool_call_id="call-1")
        with patch("hermes_cli.lifecycle.invoke_hook",
                   lambda event, **values: [hooks[event](**values)]):
            self.assertEqual(plugins._dispatch_pre_tool_call_hooks(
                "terminal", {"command": "echo first"}, **root), (None, None))
            unknown = {**root, "session_id": "unverified", "api_request_id": "request-2"}
            denied, _ = plugins._dispatch_pre_tool_call_hooks(
                "terminal", {"command": "echo second"}, **unknown)
            self.assertIn("unavailable", denied)
            self.assertNotEqual(middleware.run_tool_execution_middleware(
                "terminal", {"command": "echo second"}, lambda args: "executed", **unknown),
                "executed")
            hooks["post_compaction"](old_session_id="session", session_id="compressed")
            child = {**unknown, "session_id": "compressed"}
            self.assertEqual(plugins._dispatch_pre_tool_call_hooks(
                "terminal", {"command": "echo second"}, **child), (None, None))
            self.assertEqual(middleware.run_tool_execution_middleware(
                "terminal", {"command": "echo second"}, lambda args: "executed", **child),
                "executed")
        self.assertEqual(len(self.trace.read_text().splitlines()), 2)

    def test_reload_is_rejected_until_turn_finishes(self):
        hooks, _ = self.bind_gateway([("tool.before", "observe")])
        from hermes_cli import plugins

        ids = dict(session_id="session", conversation_id="session",
                   api_request_id="request-1", tool_call_id="call-1")
        with patch("hermes_cli.lifecycle.invoke_hook",
                   lambda event, **values: [hooks[event](**values)]):
            self.assertEqual(plugins._dispatch_pre_tool_call_hooks(
                "terminal", {"command": "echo first"}, **ids), (None, None))
            with self.assertRaisesRegex(RuntimeError, "active turn"):
                self.gateway_manager.unload()
            with patch.object(self.gateway_manager, "_discover_and_load_inner", lambda: None):
                with self.assertRaisesRegex(RuntimeError, "active turn"):
                    self.gateway_manager.discover_and_load(force=True)
            second = {**ids, "api_request_id": "request-2", "tool_call_id": "call-2"}
            self.assertEqual(plugins._dispatch_pre_tool_call_hooks(
                "terminal", {"command": "echo second"}, **second), (None, None))
        self.assertEqual(len(self.trace.read_text().splitlines()), 2)
        with self.assertRaisesRegex(RuntimeError, "registered session"):
            self.gateway_manager.unload()

    def test_gateway_releases_only_after_server_turn_exits(self):
        import threading
        entered, release = threading.Event(), threading.Event()

        async def run(_authority, _ref, _live, row):
            if row["request_id"] != "fixture-submission":
                return "other response", "completed"
            entered.set()
            if not await asyncio.to_thread(release.wait, 5):
                raise TimeoutError("server turn did not finish")
            return "response", "completed"

        hooks, _ = self.bind_gateway([("tool.before", "observe")], turn_run=run)
        from gateway.session_authority import SessionAuthority
        from hermes_cli import plugins

        def execute():
            return asyncio.run(SessionAuthority._execute_claimed(
                SimpleNamespace(), SimpleNamespace(session_id="session"), None,
                {"request_id": "fixture-submission"}))

        with ThreadPoolExecutor(max_workers=1) as pool:
            pending = pool.submit(execute)
            self.assertTrue(entered.wait(5))
            with self.assertRaisesRegex(RuntimeError, "differs from the active admission"):
                asyncio.run(SessionAuthority._execute_claimed(
                    SimpleNamespace(), SimpleNamespace(session_id="session"), None,
                    {"request_id": "other-submission"}))
            with self.assertRaisesRegex(RuntimeError, "active turn"):
                self.gateway_manager.unload()
            ids = dict(session_id="session", conversation_id="session",
                       api_request_id="request", tool_call_id="call")
            with patch("hermes_cli.lifecycle.invoke_hook",
                       lambda event, **values: [hooks[event](**values)]):
                self.assertEqual(plugins._dispatch_pre_tool_call_hooks(
                    "terminal", {"command": "echo checked"}, **ids), (None, None))
            release.set()
            self.assertEqual(pending.result(timeout=5), ("response", "completed"))
        with self.assertRaisesRegex(RuntimeError, "registered session"):
            self.gateway_manager.unload()

    def test_queued_turn_rebinds_at_gateway_execution(self):
        observed = []

        async def run(_authority, _ref, _live, row):
            ids = dict(session_id="session", conversation_id="session",
                       api_request_id=row.get("request_id") or row["admission_id"],
                       tool_call_id="call")
            observed.append(row.get("request_id") or row["admission_id"])
            self.assertIsNone(hooks["pre_tool_call"](
                tool_name="terminal", args={"command": "echo queued"}, **ids))
            return "response", "completed"

        hooks, _ = self.bind_gateway([("tool.before", "observe")],
                                     turn_run=run, prime_only=True)
        self.assertNotIn("lease", self.gateway_receipt)
        from gateway.session_authority import SessionAuthority
        for row in ({"request_id": "queued-submission"},
                    {"admission_id": "background-notification"}):
            result = asyncio.run(SessionAuthority._execute_claimed(
                SimpleNamespace(), SimpleNamespace(session_id="session"), None, row))
            self.assertEqual(result, ("response", "completed"))
        self.assertEqual(observed, ["queued-submission", "background-notification"])
        self.assertEqual(len(self.trace.read_text().splitlines()), 2)
        with self.assertRaisesRegex(RuntimeError, "registered session"):
            self.gateway_manager.unload()

    def test_registration_during_adopted_claim_creates_no_client_lease(self):
        import threading
        entered, release = threading.Event(), threading.Event()

        async def run(_authority, _ref, _live, _row):
            entered.set()
            if not await asyncio.to_thread(release.wait, 5):
                raise TimeoutError("adopted turn did not finish")
            return "response", "completed"

        self.bind_gateway([("tool.before", "observe")], turn_run=run, prime_only=True)
        from gateway.session_authority import SessionAuthority
        launch = __import__("os").environ["AW_HERMES_LAUNCH"]
        params = {"session_id": "session", "launch_path": launch, "token": "fixture-token"}

        def execute():
            return asyncio.run(SessionAuthority._execute_claimed(
                SimpleNamespace(), SimpleNamespace(session_id="session"), None,
                {"request_id": "queued-submission"}))

        with ThreadPoolExecutor(max_workers=1) as pool:
            pending = pool.submit(execute)
            self.assertTrue(entered.wait(5))
            self.assertNotIn("lease", self.gateway_rpc["prime"](params, session_id="session"))
            with self.assertRaisesRegex(RuntimeError, "active turn"):
                self.gateway_manager.unload()
            release.set()
            self.assertEqual(pending.result(timeout=5), ("response", "completed"))
        self.assertNotIn("lease", self.gateway_rpc["prime"](params, session_id="session"))
        with self.assertRaisesRegex(RuntimeError, "registered session"):
            self.gateway_manager.unload()

    def test_retire_waits_for_closed_drained_session_and_allows_new_launch(self):
        self.bind_gateway([("tool.before", "observe")], prime_only=True)
        launch = Path(__import__("os").environ["AW_HERMES_LAUNCH"])
        params = {"target_session_id": "session", "launch_path": str(launch),
                  "token": "fixture-token"}
        row = {"ended_at": None}
        pending = [{"status": "queued"}]
        authority = SimpleNamespace(db=SimpleNamespace(get_session=lambda _sid: row))
        with (patch("gateway.run_api._ACTIVE_RUNNER", object()),
              patch("gateway.session_authorities.active_authority", return_value=authority),
              patch("hermes_state_runtime.list_session_admissions", side_effect=lambda *_a, **_kw: pending)):
            retire = self.gateway_rpc["retire"]
            self.assertEqual(retire({**params, "target_session_id": "unregistered"}),
                             {"retired": True})
            with self.assertRaisesRegex(ValueError, "launch identity differs"):
                retire({**params, "token": "wrong-token"})
            self.assertEqual(retire(params), {"retired": False})
            row["ended_at"] = "closed"
            self.assertEqual(retire(params), {"retired": False})
            pending.clear()
            self.assertEqual(retire(params), {"retired": True})
            self.assertEqual(retire(params), {"retired": True})
            replacement = self.root / "replacement-launch.json"
            config = {"schema": "aw-hermes/v1alpha1", "revision": "a" * 40,
                      "profile": str(self.home), "cwd": str(Path.cwd()),
                      "binary": str(self.command), "binding": str(self.root / "binding.json"),
                      "ready_path": str(self.root / "ready.json"), "token": "replacement-token",
                      "hooks": [{"event": "tool.before", "step": "observe",
                                 "on_error": "report", "timeout": 3}]}
            replacement.write_text(json.dumps(config))
            replacement.chmod(0o600)
            self.assertEqual(self.gateway_rpc["prime"]({
                "session_id": "session", "launch_path": str(replacement),
                "token": "replacement-token"}, session_id="session")["token"],
                "replacement-token")
            with self.assertRaisesRegex(RuntimeError, "registered session"):
                self.gateway_manager.unload()
            row["ended_at"] = None
            self.assertEqual(retire(params), {"retired": True})
            self.assertEqual(retire({**params, "launch_path": str(replacement),
                                     "token": "replacement-token"}), {"retired": False})
            row["ended_at"] = "closed"
            self.assertEqual(retire({**params, "launch_path": str(replacement),
                                     "token": "replacement-token"}), {"retired": True})
            self.gateway_manager.unload()

    def test_gateway_keeps_lease_when_server_turn_is_unknown(self):
        async def run(_authority, _ref, _live, _row):
            raise RuntimeError("execution outcome unknown")

        self.bind_gateway([("tool.before", "observe")], turn_run=run)
        from gateway.session_authority import SessionAuthority
        with self.assertRaisesRegex(RuntimeError, "outcome unknown"):
            asyncio.run(SessionAuthority._execute_claimed(
                SimpleNamespace(), SimpleNamespace(session_id="session"), None,
                {"request_id": "fixture-submission"}))
        with self.assertRaisesRegex(RuntimeError, "active turn"):
            self.gateway_manager.unload()

    def test_prime_and_targeted_unload_share_lock_order(self):
        import threading
        self.bind_gateway([("tool.before", "observe")], prime_only=True)
        launch = __import__("os").environ["AW_HERMES_LAUNCH"]
        barrier = threading.Barrier(2)

        def prime():
            barrier.wait(timeout=5)
            return self.gateway_rpc["prime"](
                {"session_id": "session", "launch_path": launch, "token": "fixture-token"},
                session_id="session")["adapter"]

        def unload():
            barrier.wait(timeout=5)
            with self.assertRaisesRegex(RuntimeError, "registered session"):
                self.gateway_manager.unload()

        with ThreadPoolExecutor(max_workers=2) as pool:
            results = [pool.submit(prime), pool.submit(unload)]
            self.assertEqual(results[0].result(timeout=5), "hermes")
            results[1].result(timeout=5)

    def test_schema_coercion_cannot_change_checked_input(self):
        hooks, _ = self.bind_gateway([("tool.before", "observe")])
        from tools.registry import registry
        import model_tools
        from hermes_cli import plugins

        seen = []
        schema = {"name": "aw_array_probe", "parameters": {
            "type": "object", "properties": {"values": {"type": "array", "items": {"type": "string"}}}}}
        registry.register(name="aw_array_probe", toolset="aw-review", schema=schema,
                          handler=lambda values, **kwargs: seen.append(values) or "{}")
        self.addCleanup(registry.deregister, "aw_array_probe")
        ids = dict(session_id="session", conversation_id="session",
                   api_request_id="request", tool_call_id="call")
        with patch("hermes_cli.lifecycle.invoke_hook",
                   lambda event, **values: [hooks[event](**values)]):
            blocked, _ = plugins._dispatch_pre_tool_call_hooks(
                "aw_array_probe", {"values": '["checked"]'}, **ids)
        self.assertIsNone(blocked)
        result = model_tools.handle_function_call(
            "aw_array_probe", {"values": '["checked"]'}, "task", **ids,
            skip_pre_tool_call_hook=True, skip_tool_request_middleware=True,
            skip_tool_execution_middleware=True)
        self.assertIn("checked tool input changed during schema coercion", result)
        self.assertEqual(seen, [])

    def test_inline_coercion_cannot_change_checked_input(self):
        hooks, _ = self.bind_gateway([("tool.before", "observe")])
        from agent import inline_tool_executors as inline
        from hermes_cli import plugins

        agent = SimpleNamespace(session_id="session", _inherited_conversation_id="session",
                                _current_turn_id="turn", _current_api_request_id="request")
        ids = inline.tool_hook_ids(agent, "task", "call")
        with patch("hermes_cli.lifecycle.invoke_hook",
                   lambda event, **values: [hooks[event](**values)]):
            blocked, _ = plugins._dispatch_pre_tool_call_hooks(
                "todo_list", {"todos": '[{"id":"a","content":"test","status":"pending"}]'},
                **ids)
        self.assertIsNone(blocked)
        result = inline.INLINE_TOOL_EXECUTORS["todo_list"](
            agent, {"todos": '[{"id":"a","content":"test","status":"pending"}]'},
            inline.InlineToolContext(effective_task_id="task", tool_call_id="call"))
        self.assertIn("checked tool input changed during schema coercion", result)

    def test_dynamic_inline_executor_keeps_checked_input(self):
        hooks, _ = self.bind_gateway([("tool.before", "observe")])
        from agent import inline_tool_executors as inline
        from agent import agent_runtime_helpers as runtime
        from tools.registry import registry

        seen = []
        schema = {"name": "aw_inline_probe", "parameters": {
            "type": "object", "properties": {"values": {"type": "array", "items": {"type": "string"}}}}}
        registry.register(name="aw_inline_probe", toolset="aw-review", schema=schema,
                          handler=lambda values, **kwargs: "{}")
        self.addCleanup(registry.deregister, "aw_inline_probe")
        inline.INLINE_TOOL_EXECUTORS["aw_inline_probe"] = inline._coerced(
            "aw_inline_probe", lambda _agent, values, _ctx: seen.append(values) or "{}")
        self.addCleanup(inline.INLINE_TOOL_EXECUTORS.pop, "aw_inline_probe", None)
        agent = SimpleNamespace(
            session_id="session", _inherited_conversation_id="session",
            _current_turn_id="turn", _current_api_request_id="request",
            _memory_manager=None, valid_tool_names=["aw_inline_probe"],
            enabled_toolsets=None, disabled_toolsets=None)
        with patch("hermes_cli.lifecycle.invoke_hook",
                   lambda event, **values: [hooks[event](**values)]):
            result = runtime.invoke_tool(
                agent, "aw_inline_probe", {"values": '["checked"]'}, "task", "call",
                skip_tool_execution_middleware=True)
        self.assertIn("checked tool input changed during schema coercion", result)
        self.assertEqual(seen, [])

    def test_deferred_tool_bridge_checks_real_tool_input(self):
        hooks, _ = self.bind_gateway([("tool.before", "observe")])
        from tools.registry import registry
        from hermes_cli import plugins
        import model_tools

        seen = []
        registry.register(name="aw_bridge_probe", toolset="aw-review",
                          schema={"name": "aw_bridge_probe"},
                          handler=lambda value, **kwargs: seen.append(value) or "{}")
        self.addCleanup(registry.deregister, "aw_bridge_probe")
        ids = dict(session_id="session", conversation_id="session",
                   api_request_id="request", tool_call_id="call")
        outer = {"name": "aw_bridge_probe", "arguments": {"value": "checked"}}
        native_bridge = model_tools._dispatch_bridge_tool

        def bridge(name, *values, **kwargs):
            if name == "tool_call":
                return None, ("aw_bridge_probe", {"value": "checked"})
            return native_bridge(name, *values, **kwargs)

        with (patch("hermes_cli.lifecycle.invoke_hook",
                    lambda event, **values: [hooks[event](**values)]),
              patch("model_tools._dispatch_bridge_tool", side_effect=bridge)):
            blocked, _ = plugins._dispatch_pre_tool_call_hooks("tool_call", outer, **ids)
            self.assertIsNone(blocked)
            result = model_tools.handle_function_call(
                "tool_call", outer, "task", **ids,
                skip_pre_tool_call_hook=True, skip_tool_request_middleware=True)
        self.assertEqual(result, "{}")
        self.assertEqual(seen, [{"value": "checked"}])
        rows = [json.loads(line) for line in self.trace.read_text().splitlines()]
        self.assertEqual([row["payload"]["tool_name"] for row in rows],
                         ["tool_call", "aw_bridge_probe"])

    def test_concurrent_calls_after_compaction_keep_separate_checks(self):
        hooks, middleware = self.bind_gateway([("tool.before", "observe")])
        from hermes_cli import plugins

        hooks["post_compaction"](old_session_id="session", session_id="compressed")
        def call(index):
            ids = dict(session_id="compressed", conversation_id="session",
                       api_request_id=f"request-{index}", tool_call_id=f"call-{index}")
            args = {"command": f"echo {index}"}
            blocked, _ = plugins._dispatch_pre_tool_call_hooks("terminal", args, **ids)
            result = middleware.run_tool_execution_middleware(
                "terminal", args, lambda effective: effective["command"], **ids)
            return blocked, result

        with patch("hermes_cli.lifecycle.invoke_hook",
                   lambda event, **values: [hooks[event](**values)]):
            with ThreadPoolExecutor(max_workers=2) as pool:
                self.assertEqual(list(pool.map(call, range(2))),
                                 [(None, "echo 0"), (None, "echo 1")])
        self.assertEqual(len(self.trace.read_text().splitlines()), 2)

    def test_gateway_rejects_non_native_json_before_and_at_execution(self):
        hooks, middleware = self.bind_gateway([("tool.before", "observe")])
        ids = dict(session_id="session", conversation_id="session",
                   api_request_id="request", tool_call_id="call")
        invalid = [
            {"value": (1, 2)}, {1: "value"}, {"value": 1 << 64},
            {"value": float("nan")}, {"value": "\ud800"},
        ]
        for args in invalid:
            with self.subTest(args=repr(args)):
                self.assertEqual(hooks["pre_tool_call"](
                    tool_name="terminal", args=args, **ids)["action"], "block")
                self.assertNotEqual(middleware.run_tool_execution_middleware(
                    "terminal", args, lambda value: "executed", **ids), "executed")
        self.assertIsNone(hooks["pre_tool_call"](
            tool_name="terminal", args={"value": [1, 2]}, **ids))
        self.assertNotEqual(middleware.run_tool_execution_middleware(
            "terminal", {"value": (1, 2)}, lambda value: "executed", **ids), "executed")
