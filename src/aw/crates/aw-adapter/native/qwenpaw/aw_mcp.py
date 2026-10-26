"""Managed MCP tools bind native dispatch to the actual protocol call."""

from __future__ import annotations

from copy import deepcopy
from dataclasses import dataclass
from typing import Any

from mcp import ClientSession
from agentscope.message import ToolResultState
from agentscope.tool import MCPTool, ToolChunk

from .aw_resident import ResidentFailure, current_call, frozen


@dataclass(frozen=True)
class McpDispatch:
    """One native invocation authorizes one exact MCP operation."""

    tool: Any
    server: str
    name: str
    subject: str
    arguments: str
    parent: str
    sequence: int

    def payload(self) -> dict[str, Any]:
        """Expose protocol identity without credentials or connection options."""
        return {"server": self.server, "tool": self.name,
                "subject": self.subject, "parent_tool_call_id": self.parent,
                "generation": self.sequence, "identity_scope": "model_tool"}


class ManagedMcpTool(MCPTool):
    """Retain native permissions and conversion while fencing tools/call."""

    def __init__(self, native: MCPTool) -> None:
        if type(native) is not MCPTool:
            raise ValueError("AW managed MCP requires the pinned native MCPTool")
        super().__init__(mcp_name=native.mcp_name, tool=native._tool.model_copy(deep=True),
                         session=native._session, client_gen=native._client_gen,
                         middlewares=list(native._middlewares))
        self._timeout = native._timeout
        self._aw_identity = (self.name, self.mcp_name, self._tool.name,
                             frozen(self._tool.model_dump(mode="json")),
                             frozen(self.input_schema), self._session, self._client_gen)

    def _check_identity(self) -> None:
        expected = self._aw_identity
        if ((self.name, self.mcp_name, self._tool.name,
             frozen(self._tool.model_dump(mode="json")), frozen(self.input_schema)) != expected[:5]
                or self._session is not expected[5] or self._client_gen is not expected[6]):
            raise ResidentFailure("AW MCP descriptor or connection changed after binding")

    def aw_bind_invocation(self, invocation: Any) -> None:
        """Capture the actual tool selected by Toolkit before AW authorization."""
        self._check_identity()
        if invocation.call.name != self.name or getattr(invocation, "mcp_dispatch", None) is not None:
            raise ResidentFailure("AW MCP invocation binding is invalid")
        invocation.mcp_dispatch = McpDispatch(
            self, self.mcp_name, self._tool.name, self.name, invocation.arguments,
            invocation.call.id, invocation.sequence,
        )
        invocation.mcp_entered = False

    async def call(self, **kwargs: Any) -> ToolChunk:
        invocation = current_call.get()
        if invocation is None:
            raise ResidentFailure("AW MCP call lacks native invocation identity")
        try:
            invocation.check()
            self._check_identity()
            dispatch = getattr(invocation, "mcp_dispatch", None)
            if (dispatch is None or dispatch.tool is not self
                    or dispatch.arguments != frozen(kwargs)
                    or invocation.mcp_entered):
                raise ResidentFailure("AW MCP dispatch identity or approved arguments changed")
            invocation.mcp_entered = True
            # Copy before the protocol library can suspend or caller code can mutate.
            arguments = deepcopy(kwargs)
            if self._client_gen is not None:
                async with self._client_gen() as streams:
                    async with ClientSession(streams[0], streams[1]) as session:
                        await session.initialize()
                        invocation.check()
                        result = await session.call_tool(dispatch.name, arguments=arguments,
                                                         read_timeout_seconds=self._timeout)
            else:
                result = await self._session.call_tool(dispatch.name, arguments=arguments,
                                                      read_timeout_seconds=self._timeout)
            invocation.check()
            # AgentScope 2.0.10 keeps rendered content only. Preserve the complete
            # protocol envelope for structured consumers and Post authorization.
            result = result.model_copy(deep=True)
            return ToolChunk(
                content=self._convert_mcp_content_to_blocks(result.content),
                state=ToolResultState.ERROR if result.isError else ToolResultState.RUNNING,
                metadata={"aw_mcp_result": result.model_dump(mode="json", by_alias=True)},
            )
        except BaseException:
            invocation.failure = invocation.owner.failed = True
            raise


def bind_mcp_invocation(tool: Any, invocation: Any) -> None:
    """Bind only tools produced by this module, before resident authorization."""
    if type(tool) is ManagedMcpTool:
        ManagedMcpTool.aw_bind_invocation(tool, invocation)


async def managed_mcp_tools(client: Any) -> list[ManagedMcpTool]:
    """Resolve a native MCPClient into tools for the managed Agent's Toolkit.

    The application owns the client's connection lifecycle. Only tools obtained
    here gain the actual-call guard; dynamic discovery requires a new tool list.
    """
    tools = [ManagedMcpTool(tool) for tool in await client.list_tools()]
    if len({tool.name for tool in tools}) != len(tools):
        raise ValueError("AW rejected colliding MCP model tool names")
    return tools
