"""Shared real SDK loader and invocation fixture."""

from __future__ import annotations
import importlib.util
import sys
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from agentscope.agent import Agent
from agentscope.message import TextBlock, ToolCallBlock
from agentscope.tool import ToolChunk, ToolResponse


ROOT = Path(__file__).resolve().parents[5]
_BRIDGE = ROOT / "crates/aw-adapter/native/agentscope"
# aw_agentscope imports its sibling ledger module by absolute name; the direct
# unittest entrypoint must place that directory on the module search path the
# same way the launcher does for aw run.
sys.path[:0] = [str(_BRIDGE), str(_BRIDGE.parent / "qwenpaw")]
SPEC = importlib.util.spec_from_file_location(
    "aw_agentscope", _BRIDGE / "aw_agentscope.py"
)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class MiddlewareFixture(unittest.IsolatedAsyncioTestCase):
    async def execute(
        self, middleware: Any, response: ToolResponse, raw_input: str = "{}"
    ) -> list[Any]:
        self.executed = []
        self.chunk = ToolChunk(content=[TextBlock(text="partial")], is_last=False)

        async def tool(tool_call: Any) -> Any:
            self.executed.append(tool_call.id)
            yield self.chunk
            yield response

        chain = middleware if isinstance(middleware, list) else [middleware]
        for entry in chain:
            entry.hook.setdefault("budget_ms", 1000)
        invocation = MODULE.AwInvocationMiddleware({"hooks": [entry.hook for entry in chain]})
        # Scope transport has separate contracts; exercise the real stream policy here.
        invocation.config = None
        agent = SimpleNamespace(
            _acting_middlewares=[invocation, *chain],
            _acting_impl=tool,
            state=SimpleNamespace(session_id="session"),
        )
        return [
            item
            async for item in Agent._acting(
                agent, ToolCallBlock(id="call", name="tool", input=raw_input)
            )
        ]

