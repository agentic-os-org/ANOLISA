"""Real AgentScope ReAct/tool execution driving strict post-tool faults."""

from __future__ import annotations

import asyncio
import json
import os
from pathlib import Path
from typing import Any

from agentscope.agent import InjectionConfig
from agentscope.formatter import OpenAIChatFormatter
from agentscope.message import TextBlock, ToolCallBlock, UserMsg
from agentscope.middleware import MiddlewareBase
from agentscope.model import ChatModelBase, ChatResponse
from agentscope.permission import PermissionBehavior, PermissionDecision
from agentscope.state import AgentState
from agentscope.tool import ToolBase, ToolChunk, Toolkit, ToolResponse
from aw_agentscope import AwBridge

ROOT = Path(os.environ["AW_TEST_ROOT"])


class OfflineModel(ChatModelBase):
    """Produce one native tool batch then finish; never contact a provider."""

    def __init__(self, calls: list[ToolCallBlock]) -> None:
        super().__init__(None, "offline-fixture", self.Parameters(), stream=False)
        self.formatter = OpenAIChatFormatter()
        self.calls = calls
        self.round = 0

    async def __call__(self, *args: Any, **kwargs: Any) -> ChatResponse:
        self.round += 1
        with (ROOT / "model-inputs.jsonl").open("a") as output:
            output.write(json.dumps({"args": args, "kwargs": kwargs}, default=str) + "\n")
        return ChatResponse(
            content=self.calls if self.round == 1 else [TextBlock(text="done")], is_last=True
        )

    async def count_tokens(self, *args: Any, **kwargs: Any) -> int:
        return 1


class InjectTrustedIdentity(MiddlewareBase):
    """An original fixture for trusted argument composition, without business code."""

    async def on_acting(self, agent: Any, input_kwargs: dict[str, Any], next_handler: Any) -> Any:
        call = input_kwargs["tool_call"]
        args = json.loads(call.input)
        args["uid"] = agent.state.session_id
        call.input = json.dumps(args)
        async for item in next_handler(**input_kwargs):
            yield item


class LocalTool(ToolBase):
    name = "local_tool"
    description = "Local fixture tool"
    is_read_only = True
    is_concurrency_safe = True
    input_schema = {
        "type": "object",
        "properties": {"mode": {"type": "string"}, "uid": {"type": "string"}},
        "required": ["mode", "uid"],
    }

    async def check_permissions(
        self, tool_input: dict[str, Any], context: Any
    ) -> PermissionDecision:
        return PermissionDecision(behavior=PermissionBehavior.ALLOW)

    async def call(self, mode: str, uid: str) -> Any:
        with (ROOT / "executed.jsonl").open("a") as output:
            output.write(json.dumps({"uid": uid, "mode": mode}) + "\n")
        for text in ["sensi", "tive-", mode]:
            yield ToolChunk(content=[TextBlock(text=text)], is_last=False)
        yield ToolChunk(content=[TextBlock(text=f"complete-{mode}")], is_last=True)


async def main() -> None:
    bridge = AwBridge()
    calls = [
        ToolCallBlock(
            id=f"call-{mode}",
            name="local_tool",
            input=json.dumps({"mode": mode, "uid": "untrusted"}),
        )
        for mode in ["success", "timeout", "error", "invalid"]
    ]
    agent = bridge.create_agent(
        name="offline-agent",
        system_prompt="Use the fixture tools",
        model=OfflineModel(calls),
        toolkit=Toolkit(tools=[LocalTool()]),
        middlewares=[InjectTrustedIdentity()],
        state=AgentState(session_id="session-strict"),
        injection_config=InjectionConfig(inject_runtime_state=False),
    )
    async def consume() -> None:
        with (ROOT / "stream.jsonl").open("w") as output:
            async for event in agent.reply_stream(UserMsg("user", "run")):
                output.write(event.model_dump_json() + "\n")

    await asyncio.wait_for(consume(), 60)
    print(json.dumps([agent.state.model_dump(mode="json")]))


if __name__ == "__main__":
    asyncio.run(main())
