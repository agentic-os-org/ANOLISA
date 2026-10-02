"""AgentScope 1.x integration for the complete Tokenless lifecycle."""

from __future__ import annotations

import copy
import inspect
import json
from collections.abc import Mapping
from dataclasses import replace
from typing import Any

from agentscope.message import TextBlock
from agentscope.tool import Toolkit, ToolResponse
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
    ToolContract,
    build_tool_contracts,
    retrieve_tool_declaration,
)


class _TokenlessToolkit(Toolkit):
    """Toolkit that applies Tokenless to current and future registrations."""

    def __init__(self, integration: TokenlessAgentScope) -> None:
        super().__init__()
        self._integration = integration
        self._agent: Any | None = None
        self._session_id: str | None = None
        self.visible_markers: frozenset[str] = frozenset()
        self.output_optimizations: dict[str, OutputOptimization] = {}
        self._retrieve_function: Any | None = None

    def bind(self, agent: Any, session_id: str) -> None:
        """Bind execution attribution after the Agent is constructed."""
        self._agent = agent
        self._session_id = session_id

    def register_tool_function(self, tool_func: Any, *args: Any, **kwargs: Any) -> None:
        """Chain application postprocessing with Tokenless for every tool."""
        arguments = (
            inspect.signature(super().register_tool_function)
            .bind_partial(tool_func, *args, **kwargs)
            .arguments
        )
        variadic = arguments.get("kwargs")
        if isinstance(variadic, dict):
            arguments = {**variadic, **arguments}
        schema = arguments.get("json_schema")
        name = (
            schema.get("function", {}).get("name")
            if isinstance(schema, dict)
            else arguments.get("func_name") or getattr(tool_func, "__name__", None)
        )
        if name == self._integration.config.retrieve_tool_name:
            if self._retrieve_function is not None and tool_func is not self._retrieve_function:
                raise ValueError(f"Tool name {name!r} is reserved for Tokenless retrieval")
        else:
            self._integration.contract_for(name)
            kwargs["postprocess_func"] = self._wrap_postprocessor(kwargs.get("postprocess_func"))
        super().register_tool_function(tool_func, *args, **kwargs)
        if name == self._integration.config.retrieve_tool_name:
            self._retrieve_function = tool_func

    def _wrap_postprocessor(self, previous: Any) -> Any:
        async def postprocess(tool_call: dict[str, Any], response: ToolResponse) -> ToolResponse:
            if previous is not None:
                processed = previous(tool_call, response)
                if inspect.isawaitable(processed):
                    processed = await processed
                if processed is not None:
                    response = processed
            return await self._integration._after_tool(self, tool_call, response)

        return postprocess


class _ModelProxy:
    """Delegate an AgentScope model while transforming only its tool schemas."""

    def __init__(self, integration: TokenlessAgentScope, toolkit: _TokenlessToolkit, model: Any):
        object.__setattr__(self, "_integration", integration)
        object.__setattr__(self, "_toolkit", toolkit)
        object.__setattr__(self, "_model", model)

    async def __call__(self, *args: Any, **kwargs: Any) -> Any:
        tools = kwargs.get("tools")
        if tools is not None:
            retrieve_name = self._integration.config.retrieve_tool_name
            model_tools = [
                tool for tool in tools if tool.get("function", {}).get("name") != retrieve_name
            ]
            prompt = args[0] if args else kwargs.get("prompt", "")
            transformed = await self._integration.sdk.before_model(
                BeforeModelRequest(
                    tools=tuple(model_tools),
                    visible_context=json.dumps(prompt, ensure_ascii=False, default=str),
                    capabilities=BeforeModelCapabilities(
                        replace_tools=True,
                        recovery=RecoveryMethod.tool(self._integration.config.retrieve_tool_name),
                    ),
                    attribution=self._integration._attribution(),
                )
            )
            model_tools = list(transformed.tools)
            model_tools.append(self._integration._retrieve_declaration.as_function_tool())
            kwargs = dict(kwargs)
            kwargs["tools"] = model_tools
            self._toolkit.visible_markers = transformed.visible_markers
        return await self._model(*args, **kwargs)

    def __getattr__(self, name: str) -> Any:
        return getattr(self._model, name)

    def __setattr__(self, name: str, value: Any) -> None:
        if name in {"_integration", "_toolkit", "_model"}:
            object.__setattr__(self, name, value)
        else:
            setattr(self._model, name, value)


class TokenlessAgentScope:
    """Stable Tokenless entry point for AgentScope 1.x agents."""

    def __init__(
        self,
        config: TokenlessConfig | None = None,
        *,
        tool_contracts: Mapping[str, ToolContract] | None = None,
    ) -> None:
        self.config = config or TokenlessConfig()
        self.sdk = TokenlessSdk(self.config)
        self._tool_contracts = build_tool_contracts(tool_contracts)
        self._retrieve_declaration = retrieve_tool_declaration(self.config.retrieve_tool_name)
        self._installed_agent: Any | None = None
        self._session_id: str | None = None

    def create_toolkit(self) -> Toolkit:
        """Create the required Toolkit and register marker-scoped retrieval."""
        toolkit = _TokenlessToolkit(self)
        retrieve = self._build_retrieve_tool(toolkit)
        toolkit.register_tool_function(
            retrieve,
            json_schema=self._retrieve_declaration.as_function_tool(),
        )
        return toolkit

    def install(self, agent: Any, *, session_id: str) -> None:
        """Bind one Agent, its model boundary, and acting hook."""
        if not session_id:
            raise ValueError("session_id must not be empty")
        if self._installed_agent is agent:
            if self._session_id != session_id:
                raise ValueError("Tokenless session_id cannot change after installation")
            return
        if self._installed_agent is not None:
            raise ValueError("A TokenlessAgentScope instance can be installed on only one Agent")
        if not isinstance(getattr(agent, "toolkit", None), _TokenlessToolkit):
            raise TypeError("AgentScope 1.x requires integration.create_toolkit()")
        if not hasattr(agent, "model") or not hasattr(agent, "register_instance_hook"):
            raise TypeError("AgentScope 1.x integration requires a ReActAgent-compatible object")

        toolkit = agent.toolkit
        if toolkit._integration is not self:
            raise ValueError("The Agent toolkit belongs to a different Tokenless integration")
        self._installed_agent = agent
        self._session_id = session_id
        toolkit.bind(agent, session_id)
        agent.model = _ModelProxy(self, toolkit, agent.model)
        agent.register_instance_hook("pre_acting", "tokenless", self._before_acting)

    def contract_for(self, tool_name: Any) -> ToolContract:
        """Returns the explicit lifecycle contract for a registered tool."""
        if not isinstance(tool_name, str) or not tool_name:
            raise ValueError("registered tool must have a non-empty name")
        try:
            return self._tool_contracts[tool_name]
        except KeyError as error:
            raise ValueError(
                f"Tool {tool_name!r} requires an explicit Tokenless ToolContract"
            ) from error

    async def _before_acting(self, agent: Any, kwargs: dict[str, Any]) -> dict[str, Any]:
        if kwargs["tool_call"]["name"] == self.config.retrieve_tool_name:
            return kwargs
        tool_call = copy.deepcopy(kwargs["tool_call"])
        contract = self.contract_for(tool_call["name"])
        arguments = dict(tool_call.get("input") or {})
        optimization = OutputOptimization.NONE
        if contract.command_field is not None and self.config.rtk_enabled:
            transformed = await self.sdk.pre_tool(
                PreToolRequest(
                    tool_name=tool_call["name"],
                    arguments=arguments,
                    command_field=contract.command_field,
                    capabilities=PreToolCapabilities(
                        replace_arguments=True,
                        block_and_suggest=False,
                    ),
                    attribution=self._attribution(tool_call["id"]),
                )
            )
            if transformed.action is PreToolAction.BLOCK_AND_SUGGEST:
                raise RuntimeError("Core returned block_and_suggest without host capability")
            arguments = transformed.arguments
            optimization = transformed.output_optimization
        tool_call["input"] = arguments
        agent.toolkit.output_optimizations[tool_call["id"]] = optimization
        return {**kwargs, "tool_call": tool_call}

    async def _after_tool(
        self,
        toolkit: _TokenlessToolkit,
        tool_call: dict[str, Any],
        response: ToolResponse,
    ) -> ToolResponse:
        if not response.is_last:
            return response
        contract = self.contract_for(tool_call["name"])
        optimization = toolkit.output_optimizations.pop(tool_call["id"], OutputOptimization.NONE)
        replacements: dict[int, TextBlock] = {}
        extra_context: str | None = None
        for index, block in enumerate(response.content):
            if block.get("type") != "text":
                continue
            transformed = await self.sdk.post_tool(
                PostToolRequest(
                    result_kind=ResultKind.TOOL,
                    tool_name=tool_call["name"],
                    content=block.get("text", ""),
                    status=self._status(response),
                    content_origin=contract.content_origin,
                    output_optimization=optimization,
                    capabilities=PostToolCapabilities(
                        replace_output=True,
                        recovery=(
                            RecoveryMethod.tool(self.config.retrieve_tool_name)
                            if self._status(response) == ToolResultStatus.SUCCESS
                            and optimization == OutputOptimization.NONE
                            else RecoveryMethod()
                        ),
                        replace_with_text=True,
                    ),
                    attribution=self._attribution(tool_call["id"]),
                )
            )
            extra_context = extra_context or transformed.additional_context
            if transformed.output != block.get("text", ""):
                replacements[index] = TextBlock(type="text", text=transformed.output)
        content = [replacements.get(index, block) for index, block in enumerate(response.content)]
        if extra_context is not None:
            content.append(TextBlock(type="text", text=extra_context))
        if content == response.content:
            return response
        return replace(response, content=content)

    def _build_retrieve_tool(self, toolkit: _TokenlessToolkit) -> Any:
        async def retrieve(hash_or_marker: str) -> ToolResponse:
            try:
                response = await self.sdk.retrieve(
                    RetrieveRequest(
                        hash_or_marker,
                        toolkit.visible_markers,
                        self._attribution(),
                    )
                )
            except TokenlessError as error:
                return ToolResponse(content=[TextBlock(type="text", text=f"Error: {error}")])
            return ToolResponse(content=[TextBlock(type="text", text=response.payload)])

        retrieve.__name__ = self.config.retrieve_tool_name
        retrieve.__qualname__ = self.config.retrieve_tool_name
        retrieve.__doc__ = self._retrieve_declaration.description
        return retrieve

    def _attribution(self, tool_use_id: str | None = None) -> Attribution:
        if self._installed_agent is None or self._session_id is None:
            raise RuntimeError("TokenlessAgentScope must be installed before use")
        return Attribution(str(self._installed_agent.name), self._session_id, tool_use_id)

    @staticmethod
    def _status(response: ToolResponse) -> ToolResultStatus:
        if response.is_interrupted:
            return ToolResultStatus.INTERRUPTED
        if response.metadata is not None and response.metadata.get("success") is False:
            return ToolResultStatus.ERROR
        if any(
            block.get("type") == "text" and block.get("text", "").lstrip().startswith("Error:")
            for block in response.content
        ):
            return ToolResultStatus.ERROR
        return ToolResultStatus.SUCCESS
