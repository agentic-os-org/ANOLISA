"""Run with the pinned native QwenPaw environment; no model or cloud key is used."""

import asyncio
import importlib.util
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from agentscope.agent import Agent
from agentscope.message import TextBlock, ToolCallBlock, ToolResultState
from agentscope.middleware import MiddlewareBase
from agentscope.tool import ToolChunk, ToolResponse

ROOT = Path(__file__).resolve().parents[5]
SPEC = importlib.util.spec_from_file_location("aw_qwenpaw", ROOT / "adapters/qwenpaw/plugin.py")
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

    async def test_ask_signals_and_nonzero_are_explicit_errors(self):
        middleware = MODULE.AwToolMiddleware({}, hook("tool.before", "before"))
        for code, output in [(0, b'{"action":"ask"}'), (0, b'{"decision":"ask"}'), (-15, b""), (7, b"")]:
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


class RegistrationTests(unittest.TestCase):
    def test_receipt_requires_native_startup_and_preserves_factory_context(self):
        output = Path(os.environ["AW_TEST_OUTPUT"])
        with tempfile.TemporaryDirectory(dir=output) as temporary:
            root = Path(temporary)
            config = {
                "executable": "/aw", "binding": "/binding.json",
                "ready_path": str(root / "ready.json"), "ready_token": "token",
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

    def test_wrong_runtime_or_disabled_plugin_never_registers(self):
        with patch.object(MODULE, "version", return_value="wrong"), self.assertRaisesRegex(RuntimeError, "requires"):
            MODULE.AwPlugin().register(SimpleNamespace())
        with self.assertRaisesRegex(RuntimeError, "disables"):
            MODULE.AwPlugin().register(SimpleNamespace(config={"enabled": False}))


if __name__ == "__main__":
    unittest.main()
