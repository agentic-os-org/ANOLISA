#!/usr/bin/env python3
"""Exercise the installed integration with AgentScope and native Tokenless."""

from __future__ import annotations

import asyncio
import json
import re
import tempfile
from pathlib import Path
from typing import ClassVar

import agentscope
from agentscope.agent import Agent
from agentscope.message import TextBlock, ToolCallBlock
from agentscope.model import ChatResponse
from agentscope.permission import PermissionBehavior, PermissionDecision
from agentscope.tool import ToolBase, ToolChunk, Toolkit
from anolisa_tokenless import ContentOrigin
from tokenless_agentscope import TokenlessAgentScope, TokenlessConfig, ToolContract

_RECOVERY_PAYLOAD = "RECOVERY_SENTINEL=世界\n" + ("内容" * 525_000) + "TRAILING_NEWLINE\n"
_SCHEMA_DESCRIPTION = "SCHEMA_SENTINEL " + ("details " * 200)
_SHELL_COMMANDS: list[str] = []


class CaptureModel:
    """Capture schemas after the real Agent model middleware chain."""

    model = "capture"
    stream = False

    def __init__(self) -> None:
        self.tools: list[dict] = []

    async def __call__(
        self,
        *,
        messages: list,
        tools: list[dict],
        tool_choice: object = None,
    ) -> ChatResponse:
        del messages, tool_choice
        self.tools = tools
        return ChatResponse(content=[TextBlock(text="done")], is_last=True)


class LargeResultTool(ToolBase):
    """Return enough structured output to exercise reversible compression."""

    name = "large_result"
    description = _SCHEMA_DESCRIPTION
    input_schema: ClassVar[dict] = {
        "type": "object",
        "properties": {},
        "additionalProperties": False,
    }
    is_concurrency_safe = True
    is_read_only = True

    async def check_permissions(
        self,
        tool_input: dict,
        context: object,
    ) -> PermissionDecision:
        del tool_input, context
        return PermissionDecision(
            behavior=PermissionBehavior.ALLOW,
            message="The fixture is read-only.",
        )

    async def _execute(self) -> ToolChunk:
        payload = {
            "answer": "ORCHID-7291",
            "payload": _RECOVERY_PAYLOAD,
        }
        return ToolChunk(content=[TextBlock(text=json.dumps(payload, ensure_ascii=False))])

    async def call(self) -> ToolChunk:
        return await self._execute()

    async def __call__(self) -> ToolChunk:
        return await self._execute()


class ShellCaptureTool(ToolBase):
    """Capture the command after the real Agent acting middleware chain."""

    name = "shell"
    description = "Run a shell command."
    input_schema: ClassVar[dict] = {
        "type": "object",
        "properties": {"command": {"type": "string"}},
        "required": ["command"],
        "additionalProperties": False,
    }
    is_concurrency_safe = True
    is_read_only = True

    async def check_permissions(
        self,
        tool_input: dict,
        context: object,
    ) -> PermissionDecision:
        del tool_input, context
        return PermissionDecision(
            behavior=PermissionBehavior.ALLOW,
            message="The fixture only captures its input.",
        )

    async def _execute(self, command: str) -> ToolChunk:
        _SHELL_COMMANDS.append(command)
        return ToolChunk(content=[TextBlock(text="shell-ok")])

    async def call(self, command: str) -> ToolChunk:
        return await self._execute(command)

    async def __call__(self, command: str) -> ToolChunk:
        return await self._execute(command)


class ExistingRetrieveTool(ToolBase):
    """Represent an application tool that already uses the default name."""

    name = "tokenless_retrieve"
    description = "Existing application retrieval tool."
    input_schema: ClassVar[dict] = {
        "type": "object",
        "properties": {},
        "additionalProperties": False,
    }
    is_concurrency_safe = True
    is_read_only = True

    async def check_permissions(
        self,
        tool_input: dict,
        context: object,
    ) -> PermissionDecision:
        del tool_input, context
        return PermissionDecision(
            behavior=PermissionBehavior.ALLOW,
            message="The fixture is read-only.",
        )

    async def _execute(self) -> ToolChunk:
        return ToolChunk(content=[TextBlock(text="existing")])

    async def call(self) -> ToolChunk:
        return await self._execute()

    async def __call__(self) -> ToolChunk:
        return await self._execute()


async def main() -> None:
    """Run one real AgentScope middleware and retrieval cycle."""
    version = tuple(int(part) for part in agentscope.__version__.split(".")[:3])
    assert (2, 0, 0) <= version < (2, 1, 0)
    with tempfile.TemporaryDirectory(prefix="tokenless-agentscope-smoke-") as directory:
        existing_retrieve = ExistingRetrieveTool()
        integration = TokenlessAgentScope(
            TokenlessConfig(
                data_dir=Path(directory),
                retrieve_tool_name="tenant_tokenless_retrieve",
            ),
            tool_contracts={
                "large_result": ToolContract(ContentOrigin.API_RESPONSE),
                "tokenless_retrieve": ToolContract(ContentOrigin.API_RESPONSE),
            },
        )
        middleware_tool = integration.tools[0]
        app_toolkit = Toolkit(tools=[existing_retrieve, *integration.tools])
        assert await app_toolkit.get_tool("tokenless_retrieve") is existing_retrieve
        assert await app_toolkit.get_tool("tenant_tokenless_retrieve") is middleware_tool

        toolkit = Toolkit(
            tools=[
                LargeResultTool(),
                ShellCaptureTool(),
                existing_retrieve,
                *integration.tools,
            ]
        )
        assert await toolkit.get_tool("tokenless_retrieve") is existing_retrieve
        assert await toolkit.get_tool("tenant_tokenless_retrieve") is middleware_tool

        model = CaptureModel()
        agent = Agent(
            name="smoke",
            system_prompt="Exercise one deterministic tool.",
            model=model,
            toolkit=toolkit,
            middlewares=integration.middlewares,
        )

        model_input = await agent._prepare_model_input()
        await agent._call_model(**model_input)
        serialized_tools = json.dumps(model.tools, ensure_ascii=False)
        assert _SCHEMA_DESCRIPTION not in serialized_tools, serialized_tools
        assert re.search(
            r"If needed, call tool tenant_tokenless_retrieve with hash_or_marker=[0-9a-f]{24}",
            serialized_tools,
        )
        assert any(tool["function"]["name"] == "tenant_tokenless_retrieve" for tool in model.tools)

        _SHELL_COMMANDS.clear()
        shell_call = ToolCallBlock(
            id="call-shell",
            name="shell",
            input=json.dumps({"command": "grep needle file.txt"}),
        )
        shell_events = [event async for event in agent._acting(shell_call)]
        assert len(shell_events) == 2
        assert len(_SHELL_COMMANDS) == 1
        assert str(integration.middleware.sdk._rtk_path) in _SHELL_COMMANDS[0]
        assert "TOKENLESS_AGENT_ID=smoke" in _SHELL_COMMANDS[0]
        assert f"TOKENLESS_SESSION_ID={agent.state.session_id}" in _SHELL_COMMANDS[0]
        assert "TOKENLESS_TOOL_USE_ID=call-shell" in _SHELL_COMMANDS[0]
        assert f"TOKENLESS_DATA_DIR={directory}" in _SHELL_COMMANDS[0]

        tool_call = ToolCallBlock(id="call-large", name="large_result", input="{}")
        events = [event async for event in agent._acting(tool_call)]
        assert len(events) == 2
        streamed, response = events
        assert "TRAILING_NEWLINE" in streamed.content[0].text
        assert "TRAILING_NEWLINE" not in response.content[0].text
        marker = re.search(
            r"If needed, call tool [A-Za-z0-9_-]+ with hash_or_marker=([0-9a-f]{24})",
            response.content[0].text,
        )
        assert marker is not None
        assert response.id == "call-large"

        agent.state.summary = response.content

        async def model_boundary(**_kwargs):
            return None

        await integration.middleware.on_model_call(
            agent,
            {
                "messages": [response],
                "tools": [],
                "tool_choice": None,
                "current_model": object(),
            },
            model_boundary,
        )
        retrieve_call = ToolCallBlock(
            id="call-retrieve",
            name="tenant_tokenless_retrieve",
            input=json.dumps({"hash_or_marker": marker.group(1).upper()}),
        )
        retrieved = [event async for event in toolkit.call_tool(retrieve_call, agent.state)]
        assert len(retrieved) == 2
        assert retrieved[0].content[0].text == _RECOVERY_PAYLOAD

        if version < (2, 0, 3):
            try:
                integration.app_options()
            except RuntimeError:
                pass
            else:
                raise AssertionError("AgentScope before 2.0.3 App options must be rejected")
        else:
            options = integration.app_options()
            assert set(options) == {"extra_agent_middlewares"}
            middleware_factory = options["extra_agent_middlewares"]
            app_middlewares = await middleware_factory("user", "agent", "session")
            app_tools = await app_middlewares[0].list_tools()
            assert app_tools[0].name == "tenant_tokenless_retrieve"
            assert app_tools[0] is app_middlewares[0].retrieve_tool

            app_toolkit = Toolkit(tools=[LargeResultTool(), *app_tools])
            app_agent = Agent(
                name="app-smoke",
                system_prompt="Exercise App retrieval.",
                model=CaptureModel(),
                toolkit=app_toolkit,
                middlewares=app_middlewares,
            )
            app_call = ToolCallBlock(
                id="call-app-large",
                name="large_result",
                input="{}",
            )
            app_events = [event async for event in app_agent._acting(app_call)]
            app_response = app_events[-1]
            app_marker = re.search(
                r"If needed, call tool [A-Za-z0-9_-]+ with hash_or_marker=([0-9a-f]{24})",
                app_response.content[0].text,
            )
            assert app_marker is not None

            async def app_model_boundary(**_kwargs):
                return None

            await app_middlewares[0].on_model_call(
                app_agent,
                {
                    "messages": [app_response],
                    "tools": [],
                    "tool_choice": None,
                    "current_model": object(),
                },
                app_model_boundary,
            )
            app_retrieve = ToolCallBlock(
                id="call-app-retrieve",
                name="tenant_tokenless_retrieve",
                input=json.dumps({"hash_or_marker": app_marker.group(1)}),
            )
            app_retrieved = [
                event
                async for event in app_toolkit.call_tool(
                    app_retrieve,
                    app_agent.state,
                )
            ]
            assert app_retrieved[0].content[0].text == _RECOVERY_PAYLOAD

            other = await middleware_factory("user", "agent", "other-session")
            other_tools = await other[0].list_tools()
            assert other_tools[0] is other[0].retrieve_tool
            assert other[0].data_dir != app_middlewares[0].data_dir


if __name__ == "__main__":
    asyncio.run(main())
