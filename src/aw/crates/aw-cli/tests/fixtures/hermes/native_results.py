"""Exercise strict history delivery and approval with the pinned Hermes executor."""

import asyncio
import json
from pathlib import Path
import subprocess
import sys
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock, patch

from native_support import NativeFixture


class ResultTests(NativeFixture):
    def setUp(self):
        super().setUp()
        sdk = self.plugin_path.parents[3] / "sdk/python/src"
        self.enterContext(patch.object(sys, "path", [str(sdk), *sys.path]))

    def sdk(self, hook):
        from aw_hermes_plugin import aw_gateway
        from aw_hermes_plugin.aw_sdk_attach import SdkBinding

        manager = self.manager()
        ctx = self.context(manager)
        ctx._manager = manager
        ctx.register_rpc = Mock()
        ctx.on_unload = self.addCleanup
        ctx.session_cwd = lambda: str(Path.cwd())
        self.plugin.register(ctx)

        def invoke(hook, payload):
            reply = subprocess.run(
                [self.command, "hook", "--step", hook["step"]], input=json.dumps(payload).encode(),
                capture_output=True, timeout=3, check=False)
            return reply.returncode, reply.stdout, reply.stderr

        owner = SimpleNamespace(ctx=ctx, call=lambda fn, *args, timeout: fn(*args))
        task = SimpleNamespace(routes={"hooks": [hook]}, cwd=ctx.session_cwd(), invoke_native=invoke)
        binding = SdkBinding(owner, task, "session")
        self.enterContext(patch.object(aw_gateway, "_attachment", owner))
        self.enterContext(patch.object(aw_gateway, "_bound", return_value=binding))
        aw_gateway._install_guards(None)
        return manager

    def test_sdk_native_approval_reaches_the_human_gate(self):
        from hermes_cli.plugins import _get_pre_tool_call_directive_details

        hook = {"event": "tool.before", "step": "ask-rule", "on_error": "report", "budget_ms": 1000}
        manager = self.sdk(hook)
        with patch("hermes_cli.lifecycle.invoke_hook", manager.invoke_hook):
            for action in ("report", "block"):
                hook["on_error"] = action
                details = _get_pre_tool_call_directive_details(
                    "terminal", {}, session_id="session", api_request_id="request", tool_call_id="ask")
                self.assertEqual((details.action, details.message, details.rule_key),
                                 ("approve", "native human approval", "fixture-rule"))

    def test_managed_strict_results(self):
        launch = self.launch([("tool.after", "strict")])
        config = json.loads(launch.read_text())
        config["final_result"] = True
        config["hooks"][0].update(on_error="withhold_result", replace_result=True)
        launch.write_text(json.dumps(config))
        manager = self.manager()
        ctx = self.context(manager)
        ctx.on_unload = self.addCleanup
        self.plugin.register(ctx)
        self.check_history(manager)

    def test_sdk_strict_results(self):
        manager = self.sdk({"event": "tool.after", "step": "strict", "on_error": "withhold_result",
                            "budget_ms": 1000})
        self.check_history(manager)

    def check_history(self, manager):
        from agent import tool_executor

        agent = SimpleNamespace(
            session_id="session", _current_api_request_id="request", _current_turn_id="turn",
            _interrupt_requested=False, quiet_mode=False, verbose_logging=False, tool_progress_mode="off", log_prefix="",
            _touch_activity=Mock(), _should_start_quiet_spinner=lambda: False,
            _should_emit_quiet_tool_messages=lambda: False, tool_complete_callback=None,
            tool_progress_callback=None, _subdirectory_hints=SimpleNamespace(check_tool_call=lambda *_: ""),
            _tool_result_content_for_active_model=lambda _, result: result, _vprint=Mock(),
        )
        def calls(*items):
            return SimpleNamespace(tool_calls=[
                SimpleNamespace(id=key, function=SimpleNamespace(name="fixture", arguments=args))
                for key, args in items])

        def unfinished(batch):
            batch.timed_out_indices.add(0)
            agent._interrupt_requested = True
            ref = batch.parsed_calls[2].ref("task")
            ref.emit_post(agent, "before transform", status="blocked", error_type="policy")
            batch.results[2] = tool_executor._ToolOutcome(ref, "final blocked result", 0, True, True)

        messages = []
        with (patch("hermes_cli.lifecycle.has_hook", manager.has_hook),
              patch("hermes_cli.lifecycle.invoke_hook", manager.invoke_hook),
              patch.object(tool_executor, "_flush_session_db_after_tool_progress", return_value=True),
              patch.object(tool_executor._ConcurrentBatch, "run", unfinished)):
            tool_executor.execute_tool_calls_sequential(
                agent, calls(("invalid", "[]")), messages, "task", finalize=False)
            tool_executor.execute_tool_calls_concurrent(
                agent, calls(("timeout", "{}"), ("cancelled", "{}"), ("blocked", "{}"),
                             ("parallel-invalid", "[]")), messages, "task", finalize=False)
            tool_executor.execute_tool_calls_concurrent(
                agent, calls(("skipped", "{}"), ("withhold", "{}")), messages, "task", finalize=False)

        expected = [("invalid", "error"), ("timeout", "timeout"), ("cancelled", "cancelled"),
                    ("blocked", "blocked"), ("parallel-invalid", "error"),
                    ("skipped", "cancelled"), ("withhold", "cancelled")]
        events = [json.loads(row)["payload"]["extra"] for row in self.trace.read_text().splitlines()]
        self.assertEqual([(e["tool_call_id"], e["status"]) for e in events], expected)
        self.assertEqual(events[3]["result"], "final blocked result")
        self.assertEqual([e.get("error_type") for e in events[:5]],
                         ["invalid_tool_arguments", "tool_timeout", "keyboard_interrupt",
                          "policy", "invalid_tool_arguments"])
        self.assertEqual([(row["tool_call_id"], row["content"]) for row in messages],
                         [(key, "AW result withheld" if key == "withhold" else "compact")
                          for key, _ in expected])

    def test_sdk_exit_two_keeps_denial_and_reason(self):
        from aw_hermes_plugin.aw_sdk_attach import SdkBinding

        hook = {"event": "tool.before", "on_error": "report", "budget_ms": 1000}
        task = SimpleNamespace(routes={"hooks": [hook]}, cwd=str(self.root), invoke_native=Mock())
        owner = SimpleNamespace(call=Mock())
        binding = SdkBinding(owner, task, "session")
        values = {"tool_name": "terminal", "args": {}, "session_id": "session"}
        for stdout, reason in [
            (b'{"action":"block","message":"needs a ticket"}', "needs a ticket"),
            (b"", "AW policy blocked the tool"), (b"invalid", "AW policy blocked the tool"),
            (b'{"action":"approve"}', "AW policy blocked the tool"), (b"[]", "AW policy blocked the tool"),
        ]:
            with self.subTest(stdout=stdout):
                owner.call.return_value = (2, stdout, b"")
                directive, _ = binding.invoke(hook, None, "pre_tool_call", values)
                self.assertEqual(directive, {"action": "block", "message": reason})

    def test_sdk_before_budget_must_fit_hermes_callback_timeout(self):
        from aw_hermes_plugin.aw_sdk_attach import SdkAttachment

        owner = SimpleNamespace(verify_hooks=Mock(), turns_installed=lambda: True,
                                instance=SimpleNamespace(capabilities={"events": ["tool.before", "tool.after"]}))
        def installed(event, budget):
            return SdkAttachment.callbacks_installed(owner, {"hooks": [{"event": event, "budget_ms": budget}]})

        with patch("hermes_cli.plugins._resolve_hook_callback_timeout", return_value=30) as timeout:
            self.assertTrue(installed("tool.before", 25000))
            self.assertFalse(installed("tool.before", 25001))
            self.assertFalse(installed("tool.before", 45000))
            self.assertTrue(installed("tool.after", 55000))
            timeout.return_value = 0
            self.assertTrue(installed("tool.before", 55000))

    def test_sdk_release_failure_preserves_turn_and_retries_at_close(self):
        from aw_hermes_plugin import aw_sdk_attach
        from aw_sdk import Admission
        from aw_sdk.client import Task
        from gateway import session_finite, session_runtime_workers

        async def run():
            with patch.object(aw_sdk_attach, "load_descriptor", return_value={"endpoint": "/unused", "target": "hermes"}):
                owner = aw_sdk_attach.SdkAttachment(None, Mock(), "", "", "hermes", 1000)
            task = Task(owner.instance, {"identity": {}})
            task.binding = {"instance_id": "scope"}
            owner.instance.tasks.add(task)
            turns = []

            async def execute(*_):
                turns[0].scope = Admission(owner.instance, events=["tool.before"])
                turns[0].scope.task = task
                return "native result"

            with (patch.object(session_finite, "execute_finite_admission", execute),
                  patch.object(session_runtime_workers, "start_turn_worker"),
                  patch("gateway.session_managed_worker.managed_policy", return_value=None),
                  patch.object(owner.instance, "listen"),
                  patch("aw_sdk.client.rpc", new=AsyncMock(side_effect=[OSError("release"), {}])) as release):
                owner.install(lambda: None, turns.append, turns.remove, lambda: not turns)
                with self.assertLogs(level="ERROR"):
                    result = await session_finite.execute_finite_admission(
                        SimpleNamespace(), SimpleNamespace(session_id="session"), {"admission_id": "turn"})
                self.assertEqual(result, "native result")
                self.assertFalse(turns)
                self.assertTrue(owner.failed.is_set())
                self.assertIn(task, owner.instance.tasks)
                await owner.close()
                self.assertTrue(task.closed)
                self.assertEqual(release.await_count, 2)

        asyncio.run(run())
