"""Dependency-free boundary tests; native SDK acceptance is a separate suite."""
import asyncio
import json
import sys
import types
import unittest
from pathlib import Path
from contextlib import aclosing
from contextvars import ContextVar
from unittest.mock import patch


class Msg:
    def __init__(self, role, content):
        self.role, self.content = role, content


class TextBlock:
    def __init__(self, text):
        self.text = text


class Agent:
    """Minimal native callback seam, deliberately no SDK adoption claim."""
    def __init__(self, *, state, middlewares):
        self.state, self.middlewares = state, middlewares
        self.model_calls = 0

    async def reply_stream(self, inputs=None, structured_schema=None, yield_final_msg=False):
        async def model():
            self.model_calls += 1
            yield "model-result"
        async for item in self.middlewares[0].on_reply(self, {"inputs": inputs}, model):
            yield item

    async def reply(self, inputs=None, structured_schema=None):
        result = None
        async for item in Agent.reply_stream(self, inputs, structured_schema):
            result = item
        return result


for name, members in {
    "agentscope": {}, "agentscope.agent": {"Agent": Agent},
    "agentscope.message": {"Msg": Msg, "TextBlock": TextBlock},
    "agentscope.middleware": {"MiddlewareBase": object},
    "agentscope.event": {name: type(name, (), {}) for name in [
        "ExternalExecutionResultEvent", "UserConfirmResultEvent", "UserInterruptEvent"]},
}.items():
    module = types.ModuleType(name)
    module.__dict__.update(members)
    sys.modules[name] = module


class Diagnostic:
    def __init__(self, *args):
        pass
    @staticmethod
    def _diagnostic(data):
        pass
    def _log_failure(self, error):
        pass


module = types.ModuleType("aw_agentscope")
module.AwToolMiddleware = Diagnostic
module._encode_payload = json.dumps
sys.modules["aw_agentscope"] = module
NATIVE = Path(__file__).resolve().parents[5] / "crates/aw-adapter/native"
sys.path[:0] = [str(NATIVE / "agentscope"), str(NATIVE / "qwenpaw")]
import aw_submission as bridge
from aw_scope_ledger import AwTaskScope, CallbackFailure
import aw_scope_ledger as ledger


class Scope:
    def __init__(self):
        self.closed = False
        self.execution = None
        self.cleanup_task = None
        self.tasks = set()
        self.reservation = None
        self.calls = []
        self.mode = "allow"
        self.entered = asyncio.Event()
    async def invoke(self, hook, payload, timeout):
        self.calls.append((hook, payload, timeout))
        self.entered.set()
        if self.mode == "cancel":
            await asyncio.Future()
        if self.mode == "timeout":
            raise TimeoutError()
        return {"allow": (0, b"", b""), "deny": (2, b"", b""),
                "error": (1, b"", b""), "invalid": (0, b"{}", b"")}[self.mode]
    async def release(self):
        pass
    async def close(self):
        await asyncio.sleep(.001)
        self.closed = True


class Tests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.scopes = []
        async def open_scope(config, execution_factory=None):
            scope = Scope()
            scope.mode = self.mode
            scope.config = config
            self.scopes.append(scope)
            return scope
        self.mode = "allow"
        self.patch = patch.object(bridge, "open_turn_scope", open_scope)
        self.patch.start()
        self.addCleanup(self.patch.stop)
        self.hooks = [{"event": "input.submit", "step": name, "on_error": "block", "budget_ms": 1000}
                      for name in ["prompt", "pii"]]
        self.config = {"hooks": self.hooks}
        self.agent = self.make("session")

    def make(self, session):
        return bridge.AwSubmissionAgent(aw_config=self.config,
            state=types.SimpleNamespace(session_id=session),
            middlewares=[bridge.AwSubmissionMiddleware(self.config)])

    def user(self, text="hello"):
        return Msg("user", [TextBlock(text)])

    async def test_steps_share_identity_and_budget_turns_recover(self):
        await self.agent.reply(self.user())
        first = self.scopes[-1]
        self.assertTrue(first.closed)
        self.assertEqual(first.calls[0][1], first.calls[1][1])
        self.assertGreaterEqual(first.calls[0][2], first.calls[1][2])
        self.mode = "deny"
        with self.assertRaises(bridge.SubmissionDenied):
            await self.agent.reply(self.user())
        self.assertTrue(self.scopes[-1].closed)
        self.assertEqual(self.agent.model_calls, 1)
        self.mode = "allow"
        await self.agent.reply(self.user())
        self.assertNotEqual(first.calls[0][1]["aw_turn_id"], self.scopes[-1].calls[0][1]["aw_turn_id"])

    async def test_failure_policies(self):
        for mode in ["error", "invalid", "timeout"]:
            for policy in ["block", "report"]:
                with self.subTest(mode=mode, policy=policy):
                    self.mode = mode
                    for hook in self.hooks:
                        hook["on_error"] = policy
                    before = self.agent.model_calls
                    if policy == "block":
                        with self.assertRaises(bridge.SubmissionDenied):
                            await self.agent.reply(self.user())
                    else:
                        await self.agent.reply(self.user())
                    self.assertEqual(self.agent.model_calls - before, int(policy == "report"))
                    self.assertTrue(self.scopes[-1].closed)
                    self.assertEqual(len(self.scopes[-1].calls), 2 if policy == "report" else 1)

    async def test_encoding_failure_obeys_each_hook_policy(self):
        for policies in [("report", "report"), ("block", "report"), ("report", "block")]:
            with self.subTest(policies=policies):
                for hook, policy in zip(self.hooks, policies):
                    hook["on_error"] = policy
                before = self.agent.model_calls
                with patch.object(module, "_encode_payload", side_effect=CallbackFailure("native_input_limit")) as encode:
                    if "block" in policies:
                        with self.assertRaises(bridge.SubmissionDenied):
                            await self.agent.reply(self.user("x" * 1024 * 1024))
                    else:
                        await self.agent.reply(self.user("x" * 1024 * 1024))
                self.assertEqual(self.agent.model_calls - before, int("block" not in policies))
                self.assertEqual(encode.call_count, 1 if policies[0] == "block" else 2)
                self.assertEqual(self.scopes[-1].calls, [])
                self.assertTrue(self.scopes[-1].closed)

    async def test_shapes_history_and_system(self):
        inputs = [Msg("system", [TextBlock("system")]), Msg("assistant", [TextBlock("history")]),
                  Msg("assistant", [object()]), self.user("a"), self.user("b")]
        await self.agent.reply(inputs)
        self.assertEqual(self.scopes[-1].calls[0][1]["input_text"], "a\nb")
        for value in [None, [], inputs[0], inputs[1], inputs[2]]:
            await self.agent.reply(value)
            self.assertEqual(self.scopes[-1].calls, [])
        for value in ["string", object(), [self.user(), object()], Msg("user", [object()])]:
            with self.assertRaises(ValueError):
                await self.agent.reply(value)
            self.assertTrue(self.scopes[-1].closed)

    async def test_cancel_concurrent_and_recovery(self):
        self.mode = "cancel"
        task = asyncio.create_task(self.agent.reply(self.user()))
        while not self.scopes:
            await asyncio.sleep(0)
        await self.scopes[-1].entered.wait()
        with self.assertRaises(CallbackFailure):
            await self.agent.reply(self.user())
        task.cancel()
        await asyncio.sleep(0)
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertTrue(self.scopes[-1].closed)
        self.assertEqual(self.agent.model_calls, 0)
        self.mode = "allow"
        await self.agent.reply(self.user())

    async def test_cancelled_open_collects_handle_and_closes_before_return(self):
        entered, release = asyncio.Event(), asyncio.Event()
        scopes = []
        async def open_scope(scope):
            scopes.append(scope)
            entered.set()
            await release.wait()
            scope.handle = {"committed": True}
        async def command(scope, action, *args):
            self.assertEqual(action, "close")
            self.assertIsNotNone(scope.handle)
            return 0, b"", b""
        with patch.object(AwTaskScope, "open", open_scope), patch.object(AwTaskScope, "_command", command):
            task = asyncio.create_task(ledger.open_turn_scope({}))
            await entered.wait()
            task.cancel()
            await asyncio.sleep(0)
            self.assertFalse(task.done())
            release.set()
            task.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await task
        self.assertTrue(scopes[0].closed)
        self.assertNotIn(scopes[0], ledger._unclosed_scopes)

    async def test_scope_cancels_tools_before_releasing_ledger(self):
        scope = AwTaskScope({})
        scope.handle = {"fixture": True}
        scope.invocations["call"] = 1
        stopped = asyncio.Event()
        entered = asyncio.Event()
        async def tool():
            try:
                entered.set()
                await asyncio.Future()
            finally:
                stopped.set()
        task = asyncio.create_task(tool())
        scope.tasks.add(task)
        await entered.wait()
        commands = []
        async def command(action, *args):
            self.assertTrue(stopped.is_set())
            commands.append(action)
            return 0, b"", b""
        scope._command = command
        await scope.close()
        self.assertTrue(scope.closed)
        self.assertTrue(task.done())
        self.assertEqual(scope.invocations, {})
        self.assertEqual(commands, ["finish", "close"])

    async def test_scope_waits_for_already_cancelled_tool_teardown(self):
        scope = AwTaskScope({})
        scope.handle = {"fixture": True}
        entered, cleaning, release = asyncio.Event(), asyncio.Event(), asyncio.Event()
        stopped = False
        async def tool():
            nonlocal stopped
            try:
                entered.set()
                await asyncio.Future()
            finally:
                cleaning.set()
                await release.wait()
                stopped = True
        async def command(action, *args):
            self.assertTrue(stopped)
            return 0, b"", b""
        scope._command = command
        task = asyncio.create_task(tool())
        scope.tasks.add(task)
        await entered.wait()
        task.cancel()
        await cleaning.wait()
        closing = asyncio.create_task(scope.close())
        await asyncio.sleep(0)
        release.set()
        await closing
        self.assertTrue(stopped)
        self.assertTrue(scope.closed)

    async def test_stream_close_drains_nested_tool_in_its_context(self):
        cleaning, release, stopped = asyncio.Event(), asyncio.Event(), asyncio.Event()
        identity = ContextVar("test_invocation")
        async def tool():
            token = identity.set("tool")
            try:
                yield "partial"
                await asyncio.Future()
            finally:
                cleaning.set()
                await release.wait()
                identity.reset(token)
                stopped.set()
        async def toolkit():
            try:
                async for item in tool():
                    yield item
            except asyncio.CancelledError:
                yield "interrupted"
            finally:
                yield "final-response"
        async def native(agent, *args):
            async for item in toolkit():
                yield item
        async def consume():
            async with aclosing(self.agent.reply_stream(self.user())) as stream:
                self.assertEqual(await anext(stream), "partial")
        with patch.object(Agent, "reply_stream", native):
            consumer = asyncio.create_task(consume())
            try:
                await asyncio.wait_for(cleaning.wait(), 1)
                self.assertTrue(self.agent._aw_busy)
                self.assertFalse(self.scopes[-1].closed)
                with self.assertRaises(CallbackFailure):
                    await self.agent.reply(self.user())
                consumer.cancel()
                await asyncio.sleep(0)
                consumer.cancel()
                release.set()
                with self.assertRaises(asyncio.CancelledError):
                    await asyncio.wait_for(consumer, 1)
            finally:
                release.set()
                await asyncio.gather(consumer, return_exceptions=True)
        self.assertTrue(stopped.is_set())
        self.assertTrue(self.scopes[-1].closed)
        await self.agent.reply(self.user())

    async def test_stream_demand_and_native_error(self):
        steps = []
        async def native(agent, *args):
            steps.append("first")
            yield "first"
            steps.append("second")
            yield "second"
            raise ValueError("native failure")
        with patch.object(Agent, "reply_stream", native):
            async with aclosing(self.agent.reply_stream(self.user())) as stream:
                self.assertEqual(await anext(stream), "first")
                await asyncio.sleep(0)
                self.assertEqual(steps, ["first"])
                self.assertEqual(await anext(stream), "second")
                with self.assertRaisesRegex(ValueError, "native failure"):
                    await anext(stream)
        self.assertTrue(self.scopes[-1].closed)

    async def test_normal_and_explicit_close_surface_failed_cleanup(self):
        async def fail_close(scope):
            scope.closed = False
        with patch.object(Scope, "close", fail_close):
            with self.assertRaisesRegex(CallbackFailure, "aw_scope_cleanup_failed"):
                await self.agent.reply(self.user())
            stream = self.agent.reply_stream(self.user())
            await anext(stream)
            with self.assertRaisesRegex(CallbackFailure, "aw_scope_cleanup_failed"):
                await stream.aclose()
        self.assertFalse(self.agent._aw_busy)
        await self.agent.reply(self.user())

    async def test_cancellation_survives_failed_cleanup(self):
        async def fail_close(scope):
            scope.closed = False
        self.mode = "cancel"
        with patch.object(Scope, "close", fail_close):
            task = asyncio.create_task(self.agent.reply(self.user()))
            while not self.scopes:
                await asyncio.sleep(0)
            await self.scopes[-1].entered.wait()
            task.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await task
        self.assertFalse(self.agent._aw_busy)

    async def test_native_exception_survives_failed_cleanup(self):
        async def fail_close(scope):
            scope.closed = False
        with patch.object(Scope, "close", fail_close):
            with self.assertRaises(ValueError):
                await self.agent.reply(object())
        self.assertFalse(self.agent._aw_busy)
        await self.agent.reply(self.user())

    async def test_stream_close_and_agent_isolation(self):
        stream = self.agent.reply_stream(self.user())
        await anext(stream)
        scope = self.scopes[-1]
        await stream.aclose()
        self.assertTrue(scope.closed)
        other = self.make("other")
        await asyncio.gather(self.agent.reply(self.user()), other.reply(self.user()))
        self.assertEqual({scope.calls[0][1]["session_id"] for scope in self.scopes[-2:]}, {"session", "other"})
        self.assertTrue(all(scope.closed for scope in self.scopes))


if __name__ == "__main__":
    unittest.main()
