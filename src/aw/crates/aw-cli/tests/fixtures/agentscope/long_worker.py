"""Long-call worker: 600 sequential tool calls through the AW ledger."""

from __future__ import annotations

import asyncio
import json
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

from agentscope.state import AgentState
from agentscope.formatter import OpenAIChatFormatter
from agentscope.message import TextBlock, ToolCallBlock, UserMsg
from agentscope.model import ChatModelBase, ChatResponse
from agentscope.agent import InjectionConfig
from agentscope.permission import PermissionBehavior, PermissionDecision
from agentscope.tool import ToolBase, ToolChunk, Toolkit

from aw_agentscope import AwBridge
from aw_scope_ledger import _active_scope

CALLS = 600


class OfflineModel(ChatModelBase):
    def __init__(self, calls: list[ToolCallBlock]) -> None:
        super().__init__(None, "offline-fixture", self.Parameters(), stream=False)
        self.formatter = OpenAIChatFormatter()
        self.calls = calls
        self.round = 0

    async def __call__(self, *args: Any, **kwargs: Any) -> ChatResponse:
        self.round += 1
        content = self.calls if self.round == 1 else [TextBlock(text="done")]
        return ChatResponse(content=content, is_last=True)

    async def count_tokens(self, *args: Any, **kwargs: Any) -> int:
        return 1


class CounterTool(ToolBase):
    name = "counter_tool"
    description = "Count one call"
    is_read_only = True
    # Sequential execution: registrations retire one at a time, matching the
    # long-history capacity claim.
    is_concurrency_safe = False
    input_schema = {
        "type": "object",
        "properties": {"index": {"type": "integer"}},
        "required": ["index"],
    }

    async def check_permissions(
        self, tool_input: dict[str, Any], context: Any
    ) -> PermissionDecision:
        return PermissionDecision(
            behavior=PermissionBehavior.ALLOW, message="Long-call fixture allows the tool"
        )

    async def call(self, index: int) -> Any:
        yield ToolChunk(content=[TextBlock(text=f"complete-{index}")], is_last=True)


async def main() -> None:
    bridge = AwBridge()
    calls = [
        ToolCallBlock(id=f"call-{index}", name="counter_tool", input=json.dumps({"index": index}))
        for index in range(CALLS)
    ]
    agent = bridge.create_agent(
        name="long-agent",
        system_prompt="Use the fixture tools",
        model=OfflineModel(calls),
        toolkit=Toolkit(tools=[CounterTool()]),
        state=AgentState(session_id="session-long"),
        injection_config=InjectionConfig(inject_runtime_state=False),
    )
    await asyncio.wait_for(agent.reply(UserMsg("user", "run")), 240)
    context = [message.model_dump(mode="json") for message in agent.state.context]
    # Replay the first sequence against the live ledger; it must be rejected.
    replay = {"rejected": False, "diagnostic": ""}
    scope = _active_scope()
    if scope is not None:
        code, _, stderr = await scope._command(
            "register",
            {
                "scope": scope.handle,
                "sequence": 1,
                "call_id": "replayed-call",
                "tool_name": "counter_tool",
                "input": "{}",
            },
            6,
        )
        replay = {
            "rejected": code != 0,
            "diagnostic": stderr.decode(errors="replace")[-200:],
        }
    print(json.dumps({"context": context, "replay": replay}))


if __name__ == "__main__":
    asyncio.run(main())
