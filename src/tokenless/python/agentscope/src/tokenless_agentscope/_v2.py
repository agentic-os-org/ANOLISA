"""AgentScope 2.x integration for the complete Tokenless lifecycle."""

from __future__ import annotations

import hashlib
import json
import os
from collections.abc import AsyncGenerator, Callable, Mapping
from dataclasses import replace
from pathlib import Path
from typing import Any, ClassVar

from agentscope.message import TextBlock, ToolResultState
from agentscope.middleware import MiddlewareBase
from agentscope.permission import PermissionBehavior, PermissionDecision
from agentscope.tool import ToolBase, ToolChunk, Toolkit, ToolResponse
from anolisa_tokenless import (
    Attribution,
    BeforeModelCapabilities,
    BeforeModelRequest,
    OutputOptimization,
    PostToolCapabilities,
    PostToolRequest,
    PreToolAction,
    PreToolCapabilities,
    PreToolRequest,
    RecoveryMethod,
    ResultKind,
    RetrieveRequest,
    TokenlessConfig,
    TokenlessError,
    TokenlessSdk,
    ToolResultStatus,
)

from tokenless_agentscope._contracts import (
    RetrieveToolDeclaration,
    ToolContract,
    build_tool_contracts,
    retrieve_tool_declaration,
)

_STATE_KEY = "anolisa_tokenless"


def _marker_state(state: Any, session_markers: dict[str, frozenset[str]]) -> frozenset[str]:
    middle_context = getattr(state, "middle_context", None)
    if middle_context is not None:
        values = middle_context.get(_STATE_KEY, {}).get("visible_markers", [])
        return frozenset(values)
    return session_markers.get(state.session_id, frozenset())


def _agent_state(state: Any, session_agents: dict[str, str]) -> str:
    middle_context = getattr(state, "middle_context", None)
    if middle_context is not None:
        return middle_context[_STATE_KEY]["agent_id"]
    return session_agents[state.session_id]


def _set_marker_state(
    state: Any,
    markers: frozenset[str],
    agent_id: str,
    session_markers: dict[str, frozenset[str]],
    session_agents: dict[str, str],
) -> None:
    middle_context = getattr(state, "middle_context", None)
    if middle_context is not None:
        middle_context[_STATE_KEY] = {
            "visible_markers": sorted(markers),
            "agent_id": agent_id,
        }
    else:
        session_markers[state.session_id] = markers
        session_agents[state.session_id] = agent_id


class _RetrieveToolMixin:
    """Shared retrieval behavior across the AgentScope 2.x Tool ABI change."""

    name = "tokenless_retrieve"
    description = ""
    input_schema: ClassVar[dict[str, Any]] = {}
    is_concurrency_safe = True
    is_read_only = True
    is_state_injected = True
    is_external_tool = False
    is_mcp = False
    mcp_name = None

    def __init__(
        self,
        sdk: TokenlessSdk,
        declaration: RetrieveToolDeclaration,
        session_markers: dict[str, frozenset[str]],
        session_agents: dict[str, str],
    ) -> None:
        super().__init__()
        self._sdk = sdk
        self.name = declaration.name
        self.description = declaration.description
        self.input_schema = declaration.input_schema
        self._session_markers = session_markers
        self._session_agents = session_agents

    async def check_permissions(
        self, tool_input: dict[str, Any], context: Any
    ) -> PermissionDecision:
        del tool_input, context
        return PermissionDecision(
            behavior=PermissionBehavior.ALLOW,
            message="Tokenless retrieval is a read-only, marker-scoped operation.",
        )

    async def _retrieve(self, hash_or_marker: str, state: Any) -> ToolChunk:
        attribution = Attribution(_agent_state(state, self._session_agents), state.session_id)
        try:
            response = await self._sdk.retrieve(
                RetrieveRequest(
                    hash_or_marker,
                    _marker_state(state, self._session_markers),
                    attribution,
                )
            )
        except TokenlessError as error:
            return ToolChunk(
                content=[TextBlock(text=str(error))],
                state=ToolResultState.ERROR,
            )
        return ToolChunk(content=[TextBlock(text=response.payload)])


class _LegacyRetrieveTool(_RetrieveToolMixin, ToolBase):
    """Retrieval tool for AgentScope 2.0.0 through 2.0.2."""

    async def __call__(self, hash_or_marker: str, _agent_state: Any) -> ToolChunk:
        return await self._retrieve(hash_or_marker, _agent_state)


class _ModernRetrieveTool(_RetrieveToolMixin, ToolBase):
    """Retrieval tool for AgentScope 2.0.3 and later."""

    async def call(self, hash_or_marker: str, _agent_state: Any) -> ToolChunk:
        return await self._retrieve(hash_or_marker, _agent_state)


def _new_retrieve_tool(
    sdk: TokenlessSdk,
    declaration: RetrieveToolDeclaration,
    session_markers: dict[str, frozenset[str]],
    session_agents: dict[str, str],
) -> ToolBase:
    tool_type = _ModernRetrieveTool if hasattr(ToolBase, "call") else _LegacyRetrieveTool
    return tool_type(sdk, declaration, session_markers, session_agents)


class TokenlessMiddleware(MiddlewareBase):
    """Apply all Tokenless lifecycles to AgentScope 2.x."""

    def __init__(
        self,
        *,
        data_dir: str | os.PathLike[str] | None = None,
        retrieve_tool_name: str = "tokenless_retrieve",
        rtk_enabled: bool = True,
        tool_contracts: Mapping[str, ToolContract] | None = None,
        _config: TokenlessConfig | None = None,
    ) -> None:
        config = _config or TokenlessConfig(
            data_dir=data_dir,
            retrieve_tool_name=retrieve_tool_name,
            rtk_enabled=rtk_enabled,
        )
        self.config = config
        self.data_dir = config.data_dir
        self.retrieve_tool_name = config.retrieve_tool_name
        self._tool_contracts = build_tool_contracts(tool_contracts)
        self.sdk = TokenlessSdk(config)
        self._session_markers: dict[str, frozenset[str]] = {}
        self._session_agents: dict[str, str] = {}
        self._retrieve_declaration = retrieve_tool_declaration(self.retrieve_tool_name)
        self._retrieve_tool = _new_retrieve_tool(
            self.sdk,
            self._retrieve_declaration,
            self._session_markers,
            self._session_agents,
        )

    @property
    def retrieve_tool(self) -> ToolBase:
        """Return the retrieval tool paired with this middleware runtime."""
        return self._retrieve_tool

    async def list_tools(self) -> list[ToolBase]:
        """Return the static retrieval tool owned by this middleware."""
        return [self._retrieve_tool]

    async def register_tools(self, toolkit: Toolkit) -> None:
        """Register retrieval when the installed Toolkit supports mutation."""
        existing = await toolkit.get_tool(self.retrieve_tool_name)
        if existing is self._retrieve_tool:
            return
        if existing is not None:
            raise ValueError(
                f"Toolkit already contains a different '{self.retrieve_tool_name}' tool"
            )
        add_tool = getattr(toolkit, "add_tool", None)
        if add_tool is None:
            raise RuntimeError(
                "This AgentScope Toolkit cannot be mutated; construct it with "
                "Toolkit(tools=[..., middleware.retrieve_tool])."
            )
        await add_tool(self._retrieve_tool)

    async def on_model_call(
        self,
        agent: Any,
        input_kwargs: dict[str, Any],
        next_handler: Callable[..., Any],
    ) -> Any:
        """Compress schemas and retain the exact marker authorization set."""
        tools = []
        for tool in input_kwargs["tools"]:
            name = tool.get("function", {}).get("name")
            if name == self.retrieve_tool_name:
                continue
            if name is not None:
                self.contract_for(name)
            tools.append(tool)
        agent_id = str(agent.name)
        transformed = await self.sdk.before_model(
            BeforeModelRequest(
                tools=tuple(tools),
                visible_context=json.dumps(
                    input_kwargs["messages"], ensure_ascii=False, default=str
                ),
                capabilities=BeforeModelCapabilities(
                    replace_tools=True,
                    recovery=RecoveryMethod.tool(self.retrieve_tool_name),
                ),
                attribution=Attribution(agent_id, agent.state.session_id),
            )
        )
        _set_marker_state(
            agent.state,
            transformed.visible_markers,
            agent_id,
            self._session_markers,
            self._session_agents,
        )
        tools = list(transformed.tools)
        tools.append(self._retrieve_declaration.as_function_tool())
        return await next_handler(**{**input_kwargs, "tools": tools})

    async def on_acting(
        self,
        agent: Any,
        input_kwargs: dict[str, Any],
        next_handler: Callable[..., AsyncGenerator[Any, None]],
    ) -> AsyncGenerator[Any, None]:
        """Rewrite copied calls and transform only their final response."""
        source = input_kwargs["tool_call"]
        if source.name == self.retrieve_tool_name:
            async for item in next_handler(**input_kwargs):
                yield item
            return

        contract = self.contract_for(source.name)
        arguments = json.loads(source.input)
        if not isinstance(arguments, dict):
            raise TypeError("AgentScope tool input must decode to a JSON object")
        attribution = Attribution(str(agent.name), agent.state.session_id, source.id)
        optimization = OutputOptimization.NONE
        forwarded = source
        if contract.command_field is not None and self.config.rtk_enabled:
            transformed = await self.sdk.pre_tool(
                PreToolRequest(
                    tool_name=source.name,
                    arguments=arguments,
                    command_field=contract.command_field,
                    capabilities=PreToolCapabilities(
                        replace_arguments=True,
                        block_and_suggest=False,
                    ),
                    attribution=attribution,
                )
            )
            if transformed.action is PreToolAction.BLOCK_AND_SUGGEST:
                raise RuntimeError("Core returned block_and_suggest without host capability")
            optimization = transformed.output_optimization
            forwarded = source.model_copy(
                update={
                    "input": json.dumps(
                        transformed.arguments,
                        ensure_ascii=False,
                        separators=(",", ":"),
                    )
                }
            )
        async for item in next_handler(**{**input_kwargs, "tool_call": forwarded}):
            if isinstance(item, ToolResponse):
                yield await self._after_response(
                    item,
                    source.name,
                    contract,
                    optimization,
                    attribution,
                )
            else:
                yield item

    async def _after_response(
        self,
        response: ToolResponse,
        tool_name: str,
        contract: ToolContract,
        optimization: OutputOptimization,
        attribution: Attribution,
    ) -> ToolResponse:
        replacements: dict[int, TextBlock] = {}
        extra_context: str | None = None
        for index, block in enumerate(response.content):
            if not isinstance(block, TextBlock):
                continue
            transformed = await self.sdk.post_tool(
                PostToolRequest(
                    result_kind=ResultKind.TOOL,
                    tool_name=tool_name,
                    content=block.text,
                    status=self._status(response.state),
                    content_origin=contract.content_origin,
                    output_optimization=optimization,
                    capabilities=PostToolCapabilities(
                        replace_output=True,
                        recovery=(
                            RecoveryMethod.tool(self.retrieve_tool_name)
                            if self._status(response.state) == ToolResultStatus.SUCCESS
                            and optimization == OutputOptimization.NONE
                            else RecoveryMethod()
                        ),
                        replace_with_text=True,
                    ),
                    attribution=attribution,
                )
            )
            extra_context = extra_context or transformed.additional_context
            if transformed.output != block.text:
                replacements[index] = block.model_copy(update={"text": transformed.output})
        content = [replacements.get(index, block) for index, block in enumerate(response.content)]
        if extra_context is not None:
            content.append(TextBlock(text=extra_context))
        if content == response.content:
            return response
        return response.model_copy(update={"content": content})

    def contract_for(self, tool_name: Any) -> ToolContract:
        """Returns the explicit lifecycle contract for a model-visible tool."""
        if not isinstance(tool_name, str) or not tool_name:
            raise ValueError("model-visible tool must have a non-empty name")
        try:
            return self._tool_contracts[tool_name]
        except KeyError as error:
            raise ValueError(
                f"Tool {tool_name!r} requires an explicit Tokenless ToolContract"
            ) from error

    @staticmethod
    def _status(state: ToolResultState) -> ToolResultStatus:
        return {
            ToolResultState.SUCCESS: ToolResultStatus.SUCCESS,
            ToolResultState.ERROR: ToolResultStatus.ERROR,
            ToolResultState.INTERRUPTED: ToolResultStatus.INTERRUPTED,
            ToolResultState.DENIED: ToolResultStatus.DENIED,
        }[state]


class TokenlessAgentScope:
    """Stable Tokenless entry point for AgentScope 2.x applications."""

    def __init__(
        self,
        config: TokenlessConfig | None = None,
        *,
        tool_contracts: Mapping[str, ToolContract] | None = None,
    ) -> None:
        self.config = config or TokenlessConfig()
        self._tool_contracts = tool_contracts
        self.middleware = TokenlessMiddleware(
            _config=self.config,
            tool_contracts=tool_contracts,
        )

    @property
    def tools(self) -> list[ToolBase]:
        """Return tools to include in ``Toolkit(tools=...)``."""
        return [self.middleware.retrieve_tool]

    @property
    def middlewares(self) -> list[MiddlewareBase]:
        """Return middlewares to include in the Agent constructor."""
        return [self.middleware]

    def app_options(self) -> dict[str, Callable[..., Any]]:
        """Return AgentScope App factories with isolated session storage."""
        if not hasattr(ToolBase, "call"):
            raise RuntimeError(
                "AgentScope App integration requires AgentScope 2.0.3 or later; "
                "direct Agent integration remains supported."
            )
        if self.config.data_dir is None:
            raise ValueError("TokenlessConfig.data_dir is required for AgentScope App")

        def new_middleware(user_id: str, agent_id: str, session_id: str) -> TokenlessMiddleware:
            config = replace(
                self.config,
                data_dir=self._app_data_dir(user_id, agent_id, session_id),
            )
            return TokenlessMiddleware(
                _config=config,
                tool_contracts=self._tool_contracts,
            )

        async def middleware_factory(
            user_id: str, agent_id: str, session_id: str
        ) -> list[MiddlewareBase]:
            return [new_middleware(user_id, agent_id, session_id)]

        return {"extra_agent_middlewares": middleware_factory}

    def _app_data_dir(self, user_id: str, agent_id: str, session_id: str) -> Path:
        identity = json.dumps(
            [user_id, agent_id, session_id], ensure_ascii=False, separators=(",", ":")
        )
        key = hashlib.sha256(identity.encode()).hexdigest()
        return Path(self.config.data_dir) / "agentscope-app" / key
