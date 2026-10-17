"""Offline AgentScope 2.0.6 demo with a public AW factory Bridge."""

from __future__ import annotations

import asyncio
import json
from typing import Any

from agentscope.formatter import OpenAIChatFormatter
from agentscope.message import TextBlock, ToolCallBlock, UserMsg
from agentscope.model import ChatModelBase, ChatResponse
from agentscope.permission import PermissionBehavior, PermissionDecision
from agentscope.tool import ToolBase, ToolChunk, Toolkit
from aw_agentscope import AwBridge


class OfflineModel(ChatModelBase):
    """Supply a deterministic tool call without credentials or model traffic."""

    def __init__(self) -> None:
        super().__init__(None, "offline-demo", self.Parameters(), stream=False)
        self.formatter = OpenAIChatFormatter()
        self.called = False

    async def __call__(self, *args: Any, **kwargs: Any) -> ChatResponse:
        content = (
            [TextBlock(text="done")]
            if self.called
            else [ToolCallBlock(id="demo-call", name="echo", input='{"text":"hello"}')]
        )
        self.called = True
        return ChatResponse(content=content, is_last=True)

    async def count_tokens(self, *args: Any, **kwargs: Any) -> int:
        return 1


class Echo(ToolBase):
    name = "echo"
    description = "Return the supplied text"
    is_read_only = True
    is_concurrency_safe = True
    input_schema = {
        "type": "object",
        "properties": {"text": {"type": "string"}},
        "required": ["text"],
    }

    async def check_permissions(
        self, tool_input: dict[str, Any], context: Any
    ) -> PermissionDecision:
        return PermissionDecision(behavior=PermissionBehavior.ALLOW)

    async def call(self, text: str) -> ToolChunk:
        return ToolChunk(content=[TextBlock(text=text)])


async def main() -> None:
    bridge = AwBridge()
    agent = bridge.create_agent(
        name="demo",
        system_prompt="Echo the text",
        model=OfflineModel(),
        toolkit=Toolkit(tools=[Echo()]),
    )
    await agent.reply(UserMsg("user", "run"))
    print(json.dumps(agent.state.model_dump(mode="json")))


if __name__ == "__main__":
    asyncio.run(main())
