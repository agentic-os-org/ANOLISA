"""Real SDK private execution: scope ownership, cancellation and public delivery."""

import asyncio
from contextlib import asynccontextmanager
import json
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

NATIVE = Path(__file__).resolve().parents[5] / "crates/aw-adapter/native"
sys.path[:0] = [str(NATIVE / "agentscope"), str(NATIVE / "qwenpaw")]
import aw_execution
from aw_agentscope import AwBridge
from aw_private import PrivateExecution, current_execution, execution_environment
from aw_scope_ledger import AwTaskScope, CallbackFailure
import aw_scope_ledger as ledger
from agentscope.event import ToolResultTextDeltaEvent
from agentscope.agent import InjectionConfig
from agentscope.formatter import OpenAIChatFormatter
from agentscope.message import TextBlock, ToolCallBlock, UserMsg
from agentscope.model import ChatModelBase, ChatResponse
from agentscope.permission import PermissionBehavior, PermissionDecision
from agentscope.state import AgentState
from agentscope.tool import ToolBase, ToolChunk, ToolResponse, Toolkit


def lease(secret="synthetic-private"):
    return PrivateExecution({}, {"PUBLIC": "visible"},
        lambda value: json.loads(json.dumps(value).replace(secret, "[redacted]")),
        {"AW_FIXTURE_TASK_KEY": secret})


class Model(ChatModelBase):
    def __init__(self, text="synthetic-private"):
        super().__init__(None, "fixture", self.Parameters(), stream=False)
        self.formatter = OpenAIChatFormatter()
        self.calls = 0
        self.text = text

    async def __call__(self, *args, **kwargs):
        self.calls += 1
        content = [ToolCallBlock(id="call", name="private_tool", input=json.dumps({"text": self.text}))]
        if self.calls % 2 == 0:
            content = [TextBlock(text="done")]
        return ChatResponse(content=content, is_last=True)

    async def count_tokens(self, *args, **kwargs):
        return 1


class Tool(ToolBase):
    name = "private_tool"
    description = "Task-owned credentials"
    is_read_only = True
    is_concurrency_safe = True
    input_schema = {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}

    def __init__(self, call):
        super().__init__()
        self.execute = call

    async def check_permissions(self, tool_input, context):
        return PermissionDecision(behavior=PermissionBehavior.ALLOW, message="fixture")

    async def call(self, text):
        async for item in self.execute(text):
            yield item


class Tests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.operations = []
        self.hooks = [dict(event=event, step=event, on_error="block", budget_ms=1000,
                           replace_result=False) for event in ("tool.before", "tool.after")]
        self.failure = None
        self.tool_inputs = []
        self.leases = []
        self.entered, self.release = asyncio.Event(), asyncio.Event()
        self.mode = "normal"
        self.before_deny = False
        self.commands = []

        async def command(scope, action, value, timeout, hook=None, close_event=False):
            self.commands.append((scope, action, value))
            self.operations.append(action)
            if action == self.failure:
                return 1, b"", b"unavailable"
            if action == "open":
                self.assertEqual(value["environment"], {"PUBLIC": "visible"})
                return 0, json.dumps({"id": str(id(scope))}).encode(), b""
            if action == "policy":
                return 0, json.dumps({"hooks": self.hooks}).encode(), b""
            if action == "enable":
                return 0, b'{"invocations_per_binding":32}', b""
            if action == "invoke":
                payload = json.loads(value["native_input"])
                self.assertNotIn("synthetic-private", value["native_input"])
                if self.before_deny and hook["event"] == "tool.before":
                    return 2, b"", b""
                if "effective_input" in payload:
                    registration = next(v for s, a, v in self.commands
                                        if s is scope and a == "register")
                    self.assertEqual(registration["input"], payload["effective_input"])
            return 0, b"", b""

        self.patch = patch.object(AwTaskScope, "_command", command)
        self.patch.start()

    async def asyncTearDown(self):
        ledger._unclosed_scopes.clear()
        self.patch.stop()

    @asynccontextmanager
    async def factory(self):
        self.operations.append("acquire")
        if self.mode == "acquire":
            self.entered.set()
            await self.release.wait()
        item = lease()
        self.leases.append(item)
        if self.mode == "invalid":
            item.hook_environment = {"BAD": "synthetic-private"}
        try:
            yield item
        finally:
            self.operations.append("revoke")
            if self.mode == "revoke":
                self.entered.set()
                await self.release.wait()
            if self.mode == "revoke-error":
                raise RuntimeError("synthetic-private")
            self.operations.append("revoked")

    async def tool(self, text):
        self.operations.append("tool")
        self.tool_inputs.append(text)
        self.assertEqual(execution_environment()["AW_FIXTURE_TASK_KEY"], "synthetic-private")
        self.assertNotIn("AW_FIXTURE_TASK_KEY", os.environ)
        if self.mode == "tool":
            self.entered.set()
            try:
                await asyncio.Future()
            finally:
                self.operations.append("tool-drained")
        if self.mode == "tool-error":
            raise RuntimeError("synthetic-private")
        yield ToolChunk(content=[TextBlock(id="split", text="synthetic-")], is_last=False)
        yield ToolChunk(content=[TextBlock(id="split", text="private")], is_last=True,
                        metadata={"secret": "synthetic-private"})

    def agent(self, factory=None, tool=None, model=None):
        bridge = AwBridge.__new__(AwBridge)
        bridge.config = {"resident_policy_reload": True, "hooks": self.hooks}
        bridge._ready = True
        return bridge.create_agent(name="fixture", system_prompt="Use private_tool", model=model or Model(),
            toolkit=Toolkit(tools=[Tool(tool or self.tool)]), state=AgentState(session_id="fixture-session"),
            injection_config=InjectionConfig(inject_runtime_state=False), execution_factory=factory or self.factory)

    async def test_private_stream_publishes_one_checked_final_chunk(self):
        command = AwTaskScope._command
        for mode in ("none", "report", "replace", "strict", "withhold"):
            with self.subTest(mode=mode):
                self.hooks[:] = [] if mode == "none" else [dict(
                    event="tool.after", step="after", budget_ms=1000,
                    on_error="withhold_result" if mode in {"strict", "withhold"} else "report",
                    replace_result=mode == "replace")]

                async def checked(scope, action, value, timeout, hook=None, close_event=False):
                    reply = await command(scope, action, value, timeout, hook, close_event)
                    if action == "invoke" and not close_event:
                        if mode == "withhold":
                            return 1, b"", b"fixture fault"
                        if mode == "replace":
                            return 0, json.dumps({"aw_replacement": ToolResponse(
                                content=[TextBlock(text="checked replacement")]
                            ).model_dump(mode="json")}).encode(), b""
                    return reply

                self.operations.clear()
                deltas = []
                agent = self.agent()
                with patch.object(AwTaskScope, "_command", checked):
                    async for event in agent.reply_stream(UserMsg("user", "execute")):
                        if isinstance(event, ToolResultTextDeltaEvent):
                            self.assertIn("finish", self.operations)
                            deltas.append(event.delta)
                expected = "checked replacement" if mode == "replace" else "[redacted]"
                if mode == "withhold":
                    expected = "AW post-tool check failed; result withheld by policy"
                self.assertEqual(deltas, [expected])
                results = [block for message in agent.state.model_dump(mode="json")["context"]
                           for block in message["content"] if block["type"] == "tool_result"]
                self.assertTrue(results)
                self.assertNotIn("synthetic-private", json.dumps(results))

    async def test_abandoned_child_retains_native_cleanup_and_lease_owner(self):
        entered, cleaning, release_tool = (asyncio.Event() for _ in range(3))
        self.hooks.clear()
        self.failure = "finish"

        class ConcurrentModel(Model):
            async def __call__(self, *args, **kwargs):
                self.calls += 1
                return ChatResponse(content=[
                    ToolCallBlock(id="first", name="private_tool", input='{"text":"first"}'),
                    ToolCallBlock(id="second", name="private_tool", input='{"text":"second"}'),
                ] if self.calls == 1 else [TextBlock(text="done")], is_last=True)

        async def tool(text):
            if text == "first":
                await entered.wait()
                yield ToolChunk(content=[TextBlock(text="first result")])
            else:
                entered.set()
                try:
                    await asyncio.Future()
                finally:
                    cleaning.set()
                    await release_tool.wait()
                    self.operations.append("tool-drained")

        async def consume():
            async for _ in self.agent(tool=tool, model=ConcurrentModel()).reply_stream(UserMsg("user", "execute")):
                pass

        request = asyncio.create_task(consume())
        await asyncio.wait_for(entered.wait(), 2)
        scope = next(s for s, action, _ in self.commands if action == "open")
        try:
            async with asyncio.timeout(2):
                while not scope.closed:
                    await asyncio.sleep(.001)
            request.cancel()
            await asyncio.wait_for(asyncio.gather(request, return_exceptions=True), 20)
            self.assertTrue(cleaning.is_set())
            self.assertFalse(self.leases[0]._retired)
            self.assertIsNotNone(scope.reservation)
            self.assertIn(scope, ledger._unclosed_scopes)
            self.failure = None
            next_model = Model()
            with self.assertRaisesRegex(CallbackFailure, "cleanup_failed"):
                await self.agent(model=next_model).reply(UserMsg("user", "new"))
            self.assertEqual(next_model.calls, 0)
            self.assertEqual(self.operations.count("acquire"), 1)
            self.assertNotIn("release", self.operations)
        finally:
            release_tool.set()
            await asyncio.wait_for(asyncio.gather(*list(scope.tasks), return_exceptions=True), 2)
            await ledger.close_turn_scope(scope)
        self.assertLess(self.operations.index("tool-drained"), self.operations.index("revoke"))
        self.assertLess(self.operations.index("revoked"), self.operations.index("release"))
        self.assertEqual(self.operations.count("revoke"), 1)
        self.assertNotIn(scope, ledger._unclosed_scopes)
        await self.agent().reply(UserMsg("user", "recovered"))
        self.assertEqual(self.operations.count("acquire"), 2)

    async def test_complete_result_redaction_and_original_arguments(self):
        agent = self.agent()
        await agent.reply(UserMsg("user", "execute"))
        self.assertEqual(self.tool_inputs, ["synthetic-private"])
        after = [json.loads(v["native_input"]) for _, a, v in self.commands
                 if a == "invoke" and json.loads(v["native_input"])["event"] == "tool.after"]
        self.assertTrue(after)
        self.assertNotIn("synthetic-private", json.dumps(after))
        self.assertIn("[redacted]", json.dumps(after))
        for first, second in zip(["reserve", "acquire", "open", "tool", "close", "revoked"],
                                 ["acquire", "open", "tool", "close", "revoked", "release"]):
            self.assertLess(self.operations.index(first), self.operations.index(second))
        self.assertTrue(self.leases[0]._retired)
        self.assertIsNone(current_execution.get())
        await agent.reply(UserMsg("user", "again"))
        self.assertEqual(len(self.leases), 2)

    async def test_without_input_hook_still_owns_lease(self):
        self.hooks.clear()
        await self.agent().reply(UserMsg("user", "execute"))
        self.assertEqual(self.tool_inputs, ["synthetic-private"])
        self.assertEqual(self.operations.count("revoked"), 1)

    async def test_cancelled_acquisition_waits_then_revokes_without_dispatch(self):
        self.mode = "acquire"
        task = asyncio.create_task(self.agent().reply(UserMsg("user", "execute")))
        await self.entered.wait()
        task.cancel()
        await asyncio.sleep(.01)
        self.assertFalse(task.done())
        self.assertNotIn("release", self.operations)
        self.release.set()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertNotIn("tool", self.operations)
        self.assertLess(self.operations.index("revoked"), self.operations.index("release"))

    async def test_cancelled_revocation_retains_occupancy_until_confirmed(self):
        self.mode = "revoke"
        task = asyncio.create_task(self.agent().reply(UserMsg("user", "execute")))
        await self.entered.wait()
        task.cancel()
        await asyncio.sleep(.01)
        self.assertFalse(task.done())
        self.assertNotIn("release", self.operations)
        self.release.set()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertTrue(self.leases[0]._retired)
        self.assertLess(self.operations.index("revoked"), self.operations.index("release"))

    async def test_cancelled_native_tool_drains_before_revoke(self):
        self.mode = "tool"
        task = asyncio.create_task(self.agent().reply(UserMsg("user", "execute")))
        await self.entered.wait()
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass
        self.assertLess(self.operations.index("tool-drained"), self.operations.index("revoke"))
        self.assertLess(self.operations.index("revoked"), self.operations.index("release"))

    async def test_acquisition_timeout_retires_late_lease_once(self):
        self.mode = "acquire"
        with patch.object(aw_execution, "LEASE_TIMEOUT", .02):
            with self.assertRaisesRegex(CallbackFailure, "restart_worker"):
                await self.agent().reply(UserMsg("user", "execute"))
        scope = next(iter(ledger._unclosed_scopes))
        self.release.set()
        await asyncio.wait_for(scope.execution.pending, 1)
        self.assertNotIn("tool", self.operations)
        self.assertEqual(self.operations.count("revoked"), 1)
        with self.assertRaisesRegex(CallbackFailure, "restart_worker"):
            await self.agent().reply(UserMsg("user", "again"))
        self.assertNotIn("release", self.operations)

    async def test_revocation_timeout_keeps_original_cleanup_and_reservation(self):
        self.mode = "revoke"
        with patch.object(aw_execution, "LEASE_TIMEOUT", .02):
            with self.assertRaisesRegex(CallbackFailure, "restart_worker"):
                await self.agent().reply(UserMsg("user", "execute"))
        scope = next(iter(ledger._unclosed_scopes))
        self.release.set()
        await asyncio.wait_for(scope.execution.pending, 1)
        self.assertEqual(self.operations.count("revoked"), 1)
        with self.assertRaisesRegex(CallbackFailure, "restart_worker"):
            await self.agent().reply(UserMsg("user", "again"))
        self.assertNotIn("release", self.operations)

    async def test_new_agent_does_not_reenter_another_agents_revocation(self):
        entered, release = asyncio.Event(), asyncio.Event()
        revocations = []
        @asynccontextmanager
        async def factory():
            try:
                yield lease()
            finally:
                revocations.append(True)
                entered.set()
                await release.wait()
        first = asyncio.create_task(self.agent(factory).reply(UserMsg("user", "first")))
        await asyncio.wait_for(entered.wait(), 2)
        try:
            await asyncio.wait_for(self.agent().reply(UserMsg("user", "second")), 2)
        finally:
            release.set()
            await first
        self.assertEqual(revocations, [True])

    async def test_failed_close_retries_share_cleanup_after_waiter_cancellation(self):
        entered, release = asyncio.Event(), asyncio.Event()
        revocations = []
        @asynccontextmanager
        async def factory():
            try:
                yield lease()
            finally:
                revocations.append(True)
                entered.set()
                await release.wait()
        self.failure = "close"
        with self.assertRaisesRegex(CallbackFailure, "cleanup_failed"):
            await self.agent(factory).reply(UserMsg("user", "first"))
        self.failure = None
        first = asyncio.create_task(self.agent().reply(UserMsg("user", "retry-one")))
        await asyncio.wait_for(entered.wait(), 2)
        second = asyncio.create_task(self.agent().reply(UserMsg("user", "retry-two")))
        await asyncio.sleep(.01)
        first.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await first
        self.assertEqual(revocations, [True])
        release.set()
        await asyncio.wait_for(second, 2)
        self.assertEqual(revocations, [True])
        self.assertEqual(ledger._unclosed_scopes, set())

    async def test_invalid_hook_environment_revokes_before_dispatch(self):
        self.mode = "invalid"
        with self.assertRaisesRegex(CallbackFailure, "acquisition_failed"):
            await self.agent().reply(UserMsg("user", "execute"))
        self.assertNotIn("open", self.operations)
        self.assertNotIn("tool", self.operations)
        self.assertTrue(self.leases[0]._retired)
        self.assertEqual(self.operations[-2:], ["revoked", "release"])

    async def test_failed_revocation_retains_reservation_and_fences_next_reply(self):
        self.mode = "revoke-error"
        for _ in range(2):
            with self.assertRaisesRegex(CallbackFailure, "restart_worker"):
                await self.agent().reply(UserMsg("user", "execute"))
        self.assertEqual(self.operations.count("acquire"), 1)
        self.assertNotIn("release", self.operations)
        self.assertEqual(len(ledger._unclosed_scopes), 1)
        self.assertTrue(self.leases[0]._retired)

    async def test_child_close_failure_retains_lease_until_retry(self):
        self.failure = "close"
        with self.assertRaisesRegex(CallbackFailure, "cleanup_failed"):
            await self.agent().reply(UserMsg("user", "execute"))
        self.assertNotIn("revoke", self.operations)
        self.assertNotIn("release", self.operations)
        self.failure = None
        await self.agent().reply(UserMsg("user", "execute"))
        self.assertTrue(all(item._retired for item in self.leases))

    async def test_blocked_tool_and_tool_error_retire_lease(self):
        self.before_deny = True
        await self.agent().reply(UserMsg("user", "execute"))
        self.assertNotIn("tool", self.operations)
        self.before_deny = False
        self.mode = "tool-error"
        await self.agent().reply(UserMsg("user", "execute"))
        self.assertTrue(all(item._retired for item in self.leases))
        self.assertEqual(self.operations.count("release"), 2)

    async def test_concurrent_agents_do_not_share_credentials(self):
        ready = asyncio.Event()
        entered = []
        async def run(secret):
            @asynccontextmanager
            async def factory():
                item = lease(secret)
                try:
                    yield item
                finally:
                    self.assertFalse(item._retired)
            async def tool(text):
                entered.append(secret)
                if len(entered) == 2:
                    ready.set()
                await ready.wait()
                self.assertEqual(execution_environment()["AW_FIXTURE_TASK_KEY"], secret)
                yield ToolChunk(content=[TextBlock(text=secret)])
            await self.agent(factory, tool, Model(secret)).reply(UserMsg("user", "execute"))
            self.assertIsNone(current_execution.get())
        await asyncio.wait_for(asyncio.gather(run("first-private"), run("second-private")), 10)


if __name__ == "__main__":
    unittest.main()
