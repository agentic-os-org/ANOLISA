"""Exercise the installed integration with AgentScope 1.x and Tokenless."""

from __future__ import annotations

import asyncio
import json
import re
import tempfile
from pathlib import Path
from typing import Any

import agentscope
from agentscope.agent import ReActAgent
from agentscope.formatter import FormatterBase
from agentscope.memory import InMemoryMemory
from agentscope.message import Msg, TextBlock, ToolResultBlock, ToolUseBlock
from agentscope.model import ChatModelBase, ChatResponse
from agentscope.tool import ToolResponse
from anolisa_tokenless import ContentOrigin
from tokenless_agentscope import TokenlessAgentScope, TokenlessConfig, ToolContract

_RECOVERY_PAYLOAD = "RECOVERY_SENTINEL=世界\n" + ("内容" * 525_000) + "TRAILING_NEWLINE\n"
_SCHEMA_DESCRIPTION = "SCHEMA_SENTINEL " + ("details " * 200)
_SHELL_COMMANDS: list[str] = []


class CaptureFormatter(FormatterBase):
    """Preserve enough prompt structure to exercise the model proxy."""

    async def format(self, *args: Any, **kwargs: Any) -> list[dict[str, Any]]:
        del args
        return [
            {"role": message.role, "content": str(message.content)} for message in kwargs["msgs"]
        ]


class CaptureModel(ChatModelBase):
    """Capture the schemas delivered through a real ReActAgent boundary."""

    def __init__(self) -> None:
        super().__init__(model_name="capture", stream=False)
        self.tools: list[dict] = []

    async def __call__(self, *args: Any, **kwargs: Any) -> ChatResponse:
        del args
        self.tools = kwargs.get("tools", [])
        return ChatResponse(content=[TextBlock(type="text", text="done")])


async def large_result() -> ToolResponse:
    """Return enough structured output to exercise reversible compression."""
    payload = {
        "answer": "ORCHID-7291",
        "payload": _RECOVERY_PAYLOAD,
    }
    return ToolResponse(
        content=[TextBlock(type="text", text=json.dumps(payload, ensure_ascii=False))],
    )


async def shell(command: str) -> ToolResponse:
    """Capture the command after the AgentScope pre-acting hook."""
    _SHELL_COMMANDS.append(command)
    return ToolResponse(content=[TextBlock(type="text", text="shell-ok")])


async def tokenless_retrieve() -> ToolResponse:
    """Represent an application attempt to replace Tokenless retrieval."""
    return ToolResponse(content=[TextBlock(type="text", text="application")])


async def main() -> None:
    """Run one real AgentScope 1.x postprocessor and retrieval cycle."""
    version = tuple(int(part) for part in agentscope.__version__.split(".")[:3])
    assert (1, 0, 11) <= version < (1, 1, 0)
    with tempfile.TemporaryDirectory(prefix="tokenless-agentscope-v1-smoke-") as directory:
        integration = TokenlessAgentScope(
            TokenlessConfig(
                data_dir=Path(directory),
            ),
            tool_contracts={"large_result": ToolContract(ContentOrigin.API_RESPONSE)},
        )
        toolkit = integration.create_toolkit()
        for options in ({}, {"namesake_strategy": "override"}):
            try:
                toolkit.register_tool_function(tokenless_retrieve, **options)
            except ValueError:
                pass
            else:
                raise AssertionError("Tokenless retrieval name collision was accepted")
        toolkit.register_tool_function(
            large_result,
            json_schema={
                "type": "function",
                "function": {
                    "name": "large_result",
                    "description": _SCHEMA_DESCRIPTION,
                    "parameters": {
                        "type": "object",
                        "properties": {},
                        "additionalProperties": False,
                    },
                },
            },
        )
        toolkit.register_tool_function(shell)
        memory = InMemoryMemory()
        model = CaptureModel()
        agent = ReActAgent(
            name="smoke",
            sys_prompt="Exercise deterministic Tokenless boundaries.",
            model=model,
            formatter=CaptureFormatter(),
            toolkit=toolkit,
            memory=memory,
            enable_rewrite_query=False,
        )
        integration.install(agent, session_id="smoke-session")

        await agent._reasoning()
        serialized_tools = json.dumps(model.tools, ensure_ascii=False)
        assert _SCHEMA_DESCRIPTION not in serialized_tools, serialized_tools
        assert re.search(
            r"If needed, call tool tokenless_retrieve with hash_or_marker=[0-9a-f]{24}",
            serialized_tools,
        )
        assert any(tool["function"]["name"] == "tokenless_retrieve" for tool in model.tools)

        _SHELL_COMMANDS.clear()
        await agent._acting(
            ToolUseBlock(
                type="tool_use",
                id="call-shell",
                name="shell",
                input={"command": "grep needle file.txt"},
            ),
        )
        assert len(_SHELL_COMMANDS) == 1
        assert str(integration.sdk._rtk_path) in _SHELL_COMMANDS[0]
        assert "TOKENLESS_AGENT_ID=smoke" in _SHELL_COMMANDS[0]
        assert "TOKENLESS_SESSION_ID=smoke-session" in _SHELL_COMMANDS[0]
        assert "TOKENLESS_TOOL_USE_ID=call-shell" in _SHELL_COMMANDS[0]
        assert f"TOKENLESS_DATA_DIR={directory}" in _SHELL_COMMANDS[0]

        tool_call = ToolUseBlock(
            type="tool_use",
            id="call-large",
            name="large_result",
            input={},
        )
        responses = [chunk async for chunk in await toolkit.call_tool_function(tool_call)]
        assert len(responses) == 1
        response = responses[0]
        assert "TRAILING_NEWLINE" not in response.content[0]["text"]
        marker = re.search(
            r"If needed, call tool tokenless_retrieve with hash_or_marker=([0-9a-f]{24})",
            response.content[0]["text"],
        )
        assert marker is not None

        await memory.add(
            Msg(
                name="system",
                role="system",
                content=[
                    ToolResultBlock(
                        type="tool_result",
                        id="call-large",
                        name="large_result",
                        output=response.content,
                    ),
                ],
            ),
        )
        await agent._reasoning()
        assert marker.group(1) in toolkit.visible_markers
        retrieve_call = ToolUseBlock(
            type="tool_use",
            id="call-retrieve",
            name="tokenless_retrieve",
            input={"hash_or_marker": marker.group(1).upper()},
        )
        retrieved = [chunk async for chunk in await toolkit.call_tool_function(retrieve_call)]
        assert len(retrieved) == 1
        assert retrieved[0].content[0]["text"] == _RECOVERY_PAYLOAD


if __name__ == "__main__":
    asyncio.run(main())
