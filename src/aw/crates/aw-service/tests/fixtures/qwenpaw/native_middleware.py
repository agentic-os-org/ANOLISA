"""Run with the pinned native QwenPaw environment; no model or cloud key is used."""

import asyncio
import importlib.util
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, patch

from agentscope.agent import Agent
from agentscope.message import TextBlock, ToolCallBlock, ToolResultState
from agentscope.middleware import MiddlewareBase
from agentscope.tool import ToolChunk, ToolResponse

ROOT = Path(__file__).resolve().parents[5]
SPEC = importlib.util.spec_from_file_location("aw_qwenpaw", ROOT / "crates/aw-adapter/native/qwenpaw/plugin.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def hook(event, step):
    return {"event": event, "step": step, "on_error": "report", "budget_ms": 5000}


class MiddlewareTests(unittest.IsolatedAsyncioTestCase):
    def agent(self, middlewares, tool):
        return SimpleNamespace(
            _acting_middlewares=middlewares,
            _acting_impl=tool,
            state=SimpleNamespace(session_id="native-session"),
        )

    async def test_replacement_reaches_native_acting_result_and_preserves_metadata(self):
        original = ToolResponse(content=[TextBlock(text="original")], metadata={"kept": True})
        class Probe(MODULE.AwToolMiddleware):
            async def _invoke(self, payload):
                result = payload["tool_response"]
                result["content"][0]["text"] = "compact"
                return 0, json.dumps({"aw_replacement": result}).encode(), b""
        chunk = ToolChunk(content=[TextBlock(text="partial")], is_last=False)
        async def tool(tool_call):
            yield chunk
            yield original
        agent = self.agent([Probe({}, hook("tool.after", "compress"))], tool)
        output = [item async for item in Agent._acting(agent, ToolCallBlock(id="call", name="tool", input="{}"))]
        self.assertIs(output[0], chunk)
        self.assertEqual(output[1].content[0].text, "compact")
        self.assertEqual(output[1].metadata, original.metadata)
        self.assertEqual(original.content[0].text, "original")

    async def test_observers_and_user_hook_finish_before_compression(self):
        seen = []
        original = ToolResponse(content=[TextBlock(text="original")], metadata={"kept": True})

        class Probe(MODULE.AwToolMiddleware):
            async def _invoke(self, payload):
                seen.append(self.hook["step"])
                self_outer.assertEqual(payload["tool_response"]["content"][0]["text"], "original")
                if self.hook["step"] == "user":
                    return 3, b"", b"user failure reported"
                if self.hook["step"] == "compress":
                    payload["tool_response"]["content"][0]["text"] = "compact"
                    return 0, json.dumps({"aw_replacement": payload["tool_response"]}).encode(), b""
                return 0, b"{}", b""

        self_outer = self
        async def tool(tool_call):
            yield original
        # Generated configuration registers after wrappers in reverse so native
        # onion unwinding preserves the declared observe/user/compress order.
        registrations = [Probe({}, hook("tool.after", name)) for name in ["compress", "user", "observe"]]
        output = [item async for item in Agent._acting(self.agent(registrations, tool), ToolCallBlock(id="call", name="tool", input="{}"))]
        self.assertEqual(seen, ["observe", "user", "compress"])
        self.assertEqual(output[0].content[0].text, "compact")
        self.assertEqual(output[0].metadata, original.metadata)
        self.assertEqual(original.content[0].text, "original")

    async def test_onion_coexistence_streaming_and_projection(self):
        seen = []

        class Probe(MODULE.AwToolMiddleware):
            async def _invoke(self, payload):
                seen.append((self.hook["step"], payload))
                return 0, b"arbitrary native stdout", b"diagnostic"

        class Existing(MiddlewareBase):
            async def on_acting(self, agent, input_kwargs, next_handler):
                seen.append(("existing-before", None))
                async for item in next_handler():
                    if isinstance(item, ToolResponse):
                        seen.append(("existing-after", None))
                    yield item

        chunk = ToolChunk(content=[TextBlock(text="partial")], is_last=False)
        result = ToolResponse(content=[TextBlock(text="complete")], metadata={"nested": [None, True]})

        async def tool(tool_call):
            seen.append(("tool", tool_call.id))
            yield chunk
            yield result

        registrations = [
            Existing(),
            Probe({}, hook("tool.after", "after-a")),
            Probe({}, hook("tool.after", "after-b")),
            Probe({}, hook("tool.before", "before-a")),
            Probe({}, hook("tool.before", "before-b")),
        ]
        native = ToolCallBlock(id="call-1", name="arbitrary-tool", input='{"嵌套":[true,null,{"x":2}]}')
        output = [item async for item in Agent._acting(self.agent(registrations, tool), native)]
        self.assertIs(output[0], chunk)
        self.assertIs(output[1], result)
        self.assertEqual([item[0] for item in seen], [
            "existing-before", "before-a", "before-b", "tool", "after-b", "after-a", "existing-after",
        ])
        self.assertEqual(seen[1][1]["tool_call"]["input"], native.input)
        self.assertEqual(seen[1][1]["session_id"], "native-session")
        self.assertEqual(seen[4][1]["tool_response"], result.model_dump(mode="json"))
        self.assertEqual(seen[1][1], seen[2][1])
        self.assertEqual(seen[4][1], seen[5][1])

    async def test_block_is_observed_after_without_tool_execution(self):
        observed = []

        class Probe(MODULE.AwToolMiddleware):
            async def _invoke(self, payload):
                if self.hook["event"] == "tool.before":
                    return 2, b"", b"fixture denial"
                observed.append(payload["tool_response"])
                return 0, b"", b""

        async def tool(tool_call):
            raise AssertionError("blocked tool executed")
            yield

        agent = self.agent([
            Probe({}, hook("tool.after", "after")),
            Probe({}, hook("tool.before", "before")),
        ], tool)
        output = [item async for item in Agent._acting(agent, ToolCallBlock(id="deny", name="tool", input="{}"))]
        self.assertEqual(output[0].state, ToolResultState.DENIED)
        self.assertEqual(observed[0]["state"], "denied")

    async def test_terminal_error_states_remain_native(self):
        for state in [ToolResultState.ERROR, ToolResultState.INTERRUPTED]:
            observed = []

            class Probe(MODULE.AwToolMiddleware):
                async def _invoke(self, payload):
                    observed.append(payload["tool_response"]["state"])
                    return 0, b"", b""

            response = ToolResponse(state=state)

            async def tool(tool_call):
                yield response

            agent = self.agent([Probe({}, hook("tool.after", "after"))], tool)
            output = [item async for item in Agent._acting(agent, ToolCallBlock(id="error", name="tool", input="{}"))]
            self.assertIs(output[0], response)
            self.assertEqual(observed, [state.value])

    async def test_independent_calls_remain_concurrent(self):
        entered = 0
        ready = asyncio.Event()

        class Probe(MODULE.AwToolMiddleware):
            async def _invoke(self, payload):
                nonlocal entered
                entered += 1
                if entered == 2:
                    ready.set()
                await asyncio.wait_for(ready.wait(), 1)
                return 0, b"", b""

        async def tool(tool_call):
            yield ToolResponse()

        agent = self.agent([Probe({}, hook("tool.before", "before"))], tool)

        async def run(identifier):
            return [item async for item in Agent._acting(agent, ToolCallBlock(id=identifier, name="tool", input="{}"))]

        await asyncio.wait_for(asyncio.gather(run("one"), run("two")), 2)
        self.assertEqual(entered, 2)

    async def test_arbitrary_json_exit_zero_preserves_before_execution_and_after_response(self):
        for event in ["tool.before", "tool.after"]:
            for stdout in [b'{"action":[]}', b'{"action":{}}']:
                with self.subTest(event=event, stdout=stdout):
                    entered = []
                    response = ToolResponse(content=[TextBlock(text="unchanged")])

                    class Probe(MODULE.AwToolMiddleware):
                        async def _invoke(self, payload):
                            return 0, stdout, b""

                    async def tool(tool_call):
                        entered.append(tool_call.id)
                        yield response

                    agent = self.agent([Probe({}, hook(event, "arbitrary-json"))], tool)
                    native = ToolCallBlock(id="json-output", name="tool", input="{}")
                    result = [item async for item in Agent._acting(agent, native)]
                    self.assertEqual(entered, [native.id])
                    self.assertEqual(len(result), 1)
                    self.assertIs(result[0], response)

    async def test_ask_signals_and_nonzero_are_explicit_errors(self):
        middleware = MODULE.AwToolMiddleware({}, hook("tool.before", "before"))
        for code, output in [(0, b'{"action":"ask"}'), (0, b'{"action":"approve"}'), (0, b'{"decision":"ask"}'), (-15, b""), (7, b"")]:
            async def invoke(payload):
                return code, output, b""

            middleware._invoke = invoke
            with self.assertLogs("aw_qwenpaw", level="ERROR") as logs:
                self.assertIsNone(await middleware._run({}))
            self.assertIn("on_error=report", logs.output[0])

    async def test_callback_failures_respect_report_and_block_without_swallowing_cancellation(self):
        for event in ["tool.before", "tool.after"]:
            for action in (["report", "block"] if event == "tool.before" else ["report"]):
                for failure in [TimeoutError(), FileNotFoundError()]:
                    item = hook(event, "failed")
                    item["on_error"] = action
                    middleware = MODULE.AwToolMiddleware({}, item)
                    entered = []
                    response = ToolResponse(content=[TextBlock(text="unchanged")])

                    async def invoke(payload):
                        raise failure

                    async def tool(tool_call):
                        entered.append(True)
                        yield response

                    middleware._invoke = invoke
                    agent = self.agent([middleware], tool)
                    with self.assertLogs("aw_qwenpaw", level="ERROR"):
                        result = [value async for value in Agent._acting(agent, ToolCallBlock(id="failure", name="tool", input="{}"))]
                    if action == "block":
                        self.assertFalse(entered)
                        self.assertEqual(result[0].state, ToolResultState.DENIED)
                    else:
                        self.assertEqual(entered, [True])
                        self.assertIs(result[0], response)
        async def cancelled(payload):
            raise asyncio.CancelledError()

        middleware._invoke = cancelled
        with self.assertRaises(asyncio.CancelledError):
            await middleware._run({})


class ReloadTests(unittest.IsolatedAsyncioTestCase):
    async def test_native_reply_model_wait_retains_request_until_generator_finishes(self):
        phases = []
        entered, release = asyncio.Event(), asyncio.Event()

        class Probe(MODULE.AwReloadMiddleware):
            async def lifecycle(self, phase, key):
                phases.append(phase)

        async def reply(**kwargs):
            entered.set()
            await release.wait()
            yield TextBlock(text="complete")

        middleware = Probe({})
        agent = SimpleNamespace(_reply_middlewares=[middleware], _reply_impl=reply)
        async def consume():
            return [item async for item in Agent._reply(agent)]
        task = asyncio.create_task(consume())
        try:
            await asyncio.wait_for(entered.wait(), 1)
            self.assertEqual(phases, ["request_start"])
            release.set()
            self.assertEqual((await task)[0].text, "complete")
            self.assertEqual(phases, ["request_start", "request_end"])
        finally:
            release.set()
            if not task.done():
                task.cancel()
            await asyncio.gather(task, return_exceptions=True)

    async def test_reply_failure_and_cancellation_keep_unconfirmed_request_busy(self):
        for failure in (False, True):
            phases = []
            entered = asyncio.Event()

            class Probe(MODULE.AwReloadMiddleware):
                async def lifecycle(self, phase, key):
                    phases.append(phase)

            async def reply(**kwargs):
                entered.set()
                if failure:
                    raise RuntimeError("native failure")
                await asyncio.Event().wait()
                yield

            agent = SimpleNamespace(_reply_middlewares=[Probe({})], _reply_impl=reply)
            async def consume():
                return [item async for item in Agent._reply(agent)]
            task = asyncio.create_task(consume())
            try:
                await asyncio.wait_for(entered.wait(), 1)
                if not failure:
                    task.cancel()
                with self.assertRaises(RuntimeError if failure else asyncio.CancelledError):
                    await task
                self.assertEqual(phases, ["request_start"])
            finally:
                if not task.done():
                    task.cancel()
                await asyncio.gather(task, return_exceptions=True)

    async def test_failed_reply_cleanup_does_not_report_request_end(self):
        phases = []

        class Probe(MODULE.AwReloadMiddleware):
            async def lifecycle(self, phase, key):
                phases.append(phase)

        async def reply(**kwargs):
            try:
                yield TextBlock(text="partial")
            finally:
                raise RuntimeError("reply cleanup failed")

        generator = Probe({}).on_reply(None, {}, reply)
        await anext(generator)
        with self.assertRaisesRegex(RuntimeError, "reply cleanup failed"):
            await generator.aclose()
        self.assertEqual(phases, ["request_start"])

    async def test_terminal_tool_errors_keep_unconfirmed_tool_busy(self):
        for streaming in (False, True):
            with self.subTest(streaming=streaming):
                phases = []

                class Probe(MODULE.AwReloadMiddleware):
                    async def lifecycle(self, phase, key):
                        phases.append(phase)

                async def tool(tool_call):
                    try:
                        if streaming:
                            yield ToolChunk(content=[TextBlock(text="partial")], is_last=False)
                        raise RuntimeError("terminal native failure")
                    finally:
                        await asyncio.sleep(0)
                        phases.append("generator_closed")

                agent = SimpleNamespace(_acting_middlewares=[Probe({})], _acting_impl=tool,
                                        state=SimpleNamespace(session_id="session"))
                with patch.object(MODULE.AwToolMiddleware, "_run", new=AsyncMock(return_value=None)):
                    with self.assertRaisesRegex(RuntimeError, "terminal native failure"):
                        [item async for item in Agent._acting(agent, ToolCallBlock(id="failed", name="tool", input="{}"))]
                self.assertEqual(phases, ["tool_start", "generator_closed"])

    async def test_failed_generator_cleanup_does_not_report_tool_end(self):
        phases = []

        class Probe(MODULE.AwReloadMiddleware):
            async def lifecycle(self, phase, key):
                phases.append(phase)

        async def tool(tool_call):
            try:
                yield ToolResponse(content=[TextBlock(text="result")])
            finally:
                phases.append("generator_closing")
                raise RuntimeError("native cleanup failed")

        agent = SimpleNamespace(_acting_middlewares=[Probe({})], _acting_impl=tool,
                                state=SimpleNamespace(session_id="session"))
        with patch.object(MODULE.AwToolMiddleware, "_run", new=AsyncMock(side_effect=[None, RuntimeError("after failed")])):
            with self.assertRaisesRegex(RuntimeError, "native cleanup failed"):
                [item async for item in Probe({}).on_acting(agent, {"tool_call": ToolCallBlock(id="failed", name="tool", input="{}")}, tool)]
        self.assertEqual(phases, ["tool_start", "generator_closing"])

    async def test_final_tool_response_is_held_until_native_exhaustion(self):
        for invalid_tail in (False, True):
            with self.subTest(invalid_tail=invalid_tail):
                phases = []

                class Probe(MODULE.AwReloadMiddleware):
                    async def lifecycle(self, phase, key):
                        phases.append(phase)

                async def tool(tool_call):
                    try:
                        yield ToolResponse(content=[TextBlock(text="complete")])
                        if invalid_tail:
                            yield ToolChunk(content=[TextBlock(text="invalid")], is_last=False)
                    finally:
                        phases.append("generator_closed")

                agent = SimpleNamespace(_acting_middlewares=[Probe({})], _acting_impl=tool,
                                        state=SimpleNamespace(session_id="session"))
                call = ToolCallBlock(id="complete", name="tool", input="{}")
                generator = Agent._acting(agent, call)
                with patch.object(MODULE.AwToolMiddleware, "_run", new=AsyncMock(return_value=None)):
                    try:
                        if invalid_tail:
                            with self.assertRaisesRegex(MODULE.CallbackFailure, "native_output_after_final_response"):
                                await anext(generator)
                        else:
                            self.assertIsInstance(await anext(generator), ToolResponse)
                            self.assertEqual(phases, ["tool_start", "generator_closed", "tool_end"])
                    finally:
                        await generator.aclose()
                        await asyncio.wait_for(asyncio.get_running_loop().shutdown_asyncgens(), 3)
                if invalid_tail:
                    self.assertNotIn("tool_end", phases)

    async def test_terminal_chunk_cleanup_failure_retains_claim(self):
        for state in (ToolResultState.ERROR, ToolResultState.INTERRUPTED):
            with self.subTest(state=state):
                phases = []

                class Probe(MODULE.AwReloadMiddleware):
                    async def lifecycle(self, phase, key):
                        phases.append(phase)

                async def tool(tool_call):
                    yield ToolChunk(content=[TextBlock(text="failed")], state=state, is_last=True)
                    yield ToolResponse(content=[TextBlock(text="failed")], state=state)
                    raise RuntimeError("native cleanup failed")

                agent = SimpleNamespace(_acting_middlewares=[Probe({})], _acting_impl=tool,
                                        state=SimpleNamespace(session_id="session"))
                stream = Agent._acting(agent, ToolCallBlock(id="error", name="tool", input="{}"))
                with patch.object(MODULE.AwToolMiddleware, "_run", new=AsyncMock(return_value=None)):
                    try:
                        with self.assertRaisesRegex(RuntimeError, "native cleanup failed"):
                            await anext(stream)
                    finally:
                        await stream.aclose()
                self.assertEqual(phases, ["tool_start"])
        await asyncio.wait_for(asyncio.get_running_loop().shutdown_asyncgens(), 3)

    async def test_unconfirmed_cancelled_tool_keeps_its_claim_busy(self):
        phases = []
        entered = asyncio.Event()

        class Probe(MODULE.AwReloadMiddleware):
            async def lifecycle(self, phase, key):
                phases.append(phase)

        async def tool(tool_call):
            entered.set()
            await asyncio.Event().wait()
            yield ToolResponse(content=[TextBlock(text="never completed")])

        agent = SimpleNamespace(_acting_middlewares=[Probe({})], _acting_impl=tool,
                                state=SimpleNamespace(session_id="session"))
        async def consume():
            return [item async for item in Agent._acting(agent, ToolCallBlock(id="call", name="tool", input="{}"))]
        with patch.object(MODULE.AwToolMiddleware, "_run", new=AsyncMock(return_value=None)):
            task = asyncio.create_task(consume())
            try:
                await asyncio.wait_for(entered.wait(), 1)
                task.cancel()
                with self.assertRaises(asyncio.CancelledError):
                    await task
                self.assertEqual(phases, ["tool_start"])
            finally:
                if not task.done():
                    task.cancel()
                await asyncio.gather(task, return_exceptions=True)


class RegistrationTests(unittest.TestCase):
    def test_receipt_requires_native_startup_and_preserves_factory_context(self):
        output = Path(os.environ["AW_TEST_OUTPUT"])
        with tempfile.TemporaryDirectory(dir=output) as temporary:
            root = Path(temporary)
            config = {
                "executable": "/aw",
                "binding": "/binding.json",
                "ready_path": str(root / "ready.json"),
                "ready_token": "token",
                "hooks": [hook("tool.after", "after"), hook("tool.before", "before")],
            }
            path = root / "config.json"
            path.write_text(json.dumps(config))
            factories, startup = [], []
            api = SimpleNamespace(
                config={},
                register_middleware=lambda factory, **kw: factories.append(factory),
                register_startup_hook=lambda name, callback: startup.append(callback),
            )
            with patch.dict(os.environ, AW_NATIVE_CONFIG=str(path)):
                MODULE.AwPlugin().register(api)
            self.assertFalse((root / "ready.json").exists())
            path.unlink()
            self.assertEqual([factory(None, None).hook["step"] for factory in factories], ["after", "before"])
            startup[0]()
            self.assertEqual(json.loads((root / "ready.json").read_text()), {
                "version": 1, "adapter": "qwenpaw", "pid": os.getpid(), "token": "token", "hooks": 2,
            })
            self.assertEqual((root / "ready.json").stat().st_mode & 0o777, 0o600)
            self.assertEqual(list(root.glob(".aw-ready-*")), [])

    def test_empty_hooks_register_only_startup_readiness(self):
        output = Path(os.environ["AW_TEST_OUTPUT"])
        with tempfile.TemporaryDirectory(dir=output) as temporary:
            root = Path(temporary)
            config = {
                "executable": "/aw", "binding": "/binding.json",
                "ready_path": str(root / "ready.json"), "ready_token": "token",
                "hooks": [],
            }
            path = root / "config.json"
            path.write_text(json.dumps(config))
            factories, startup = [], []
            api = SimpleNamespace(
                config={},
                register_middleware=lambda factory, **kw: factories.append(factory),
                register_startup_hook=lambda name, callback: startup.append(callback),
            )
            with patch.dict(os.environ, AW_NATIVE_CONFIG=str(path)):
                MODULE.AwPlugin().register(api)
            self.assertEqual(factories, [])
            self.assertFalse((root / "ready.json").exists())
            startup[0]()
            self.assertEqual(json.loads((root / "ready.json").read_text())["hooks"], 0)

    def test_wrong_runtime_or_disabled_plugin_never_registers(self):
        incompatible = {"qwenpaw": "2.2.2b4", "agentscope": "2.0.8"}
        with patch.object(MODULE, "version", side_effect=incompatible.__getitem__):
            with self.assertRaisesRegex(RuntimeError, "AgentScope 2.0.9"):
                MODULE.AwPlugin().register(SimpleNamespace())
        with patch.object(MODULE, "version", return_value="wrong"), self.assertRaisesRegex(RuntimeError, "requires"):
            MODULE.AwPlugin().register(SimpleNamespace())
        with self.assertRaisesRegex(RuntimeError, "disables"):
            MODULE.AwPlugin().register(SimpleNamespace(config={"enabled": False}))


if __name__ == "__main__":
    unittest.main()
