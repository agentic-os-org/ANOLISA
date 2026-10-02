"""Tokenless plugin for QwenPaw.

Registers one AgentScope middleware that runs the complete Tokenless lifecycle
in-process through ``anolisa_tokenless.TokenlessSdk``: tool schema compression
before each model call, RTK command rewriting before ``execute_shell_command``,
tool result compression after every tool call, and a marker-authorized
``tokenless_retrieve`` tool.

QwenPaw imports this file once at install time before the plugin's
``requirements.txt`` is installed, so ``anolisa_tokenless`` is imported inside
``register`` rather than at module level. Tools outside the built-in contract
table are passed through untouched.
"""

from __future__ import annotations

import json
import logging
from collections.abc import AsyncGenerator, Callable
from pathlib import Path
from typing import Any

from agentscope.message import TextBlock, ToolResultState
from agentscope.middleware import MiddlewareBase
from agentscope.tool import ToolChunk, ToolResponse

logger = logging.getLogger(__name__)

AGENT_ID = "qwenpaw"
STATE_KEY = "anolisa_tokenless"
RETRIEVE_TOOL = "tokenless_retrieve"
RETRIEVE_DECLARATION = {
    "type": "function",
    "function": {
        "name": RETRIEVE_TOOL,
        "description": (
            "Restore omitted content when needed. Pass only the 24-character hash "
            "from a visible Tokenless recovery instruction, not the whole instruction."
        ),
        "parameters": {
            "type": "object",
            "properties": {
                "hash_or_marker": {
                    "type": "string",
                    "description": (
                        "The 24-character hash from a visible recovery instruction; "
                        "historical Tokenless markers are also accepted"
                    ),
                }
            },
            "required": ["hash_or_marker"],
            "additionalProperties": False,
        },
    },
}

# tool name -> (content_origin wire value, RTK command field) for QwenPaw
# 2.2.0's built-in tools. Core passes `file_content` through untouched, so a
# tool absent from this table (skills, MCP servers, newer built-ins) falls
# back to it and is never compressed or rewritten.
_CONTRACTS: dict[str, tuple[str, str | None]] = {
    "execute_shell_command": ("command_output", "command"),
    **{
        name: ("file_content", None)
        for name in ("read_file", "recall_history", "view_image", "view_video")
    },
    **{
        name: ("api_response", None)
        for name in (
            "glob_search",
            "grep_search",
            "ast_search",
            "web_fetch",
            "web_search",
            "memory_search",
            "get_current_time",
            "get_token_usage",
            "list_agents",
            "check_agent_task",
            "chat_with_agent",
            "submit_to_agent",
            "spawn_subagent",
            "delegate_external_agent",
            "browser",
            "desktop_screenshot",
            "run_tool_batch",
            "send_file_to_user",
            "set_user_timezone",
            "activate_f1_exploration_mode",
            "edit_file",
            "write_file",
            "append_file",
            "materialize_skill",
        )
    },
}
_DEFAULT_CONTRACT: tuple[str, str | None] = ("file_content", None)

# Every name the middleware and the retrieve tool import from
# anolisa_tokenless. A released wheel that predates one of them imports fine
# at registration and would fail at the first model call instead, so the
# plugin refuses to register against such a wheel.
_REQUIRED_SDK_NAMES = (
    "Attribution",
    "BeforeModelCapabilities",
    "BeforeModelRequest",
    "ContentOrigin",
    "OutputOptimization",
    "PostToolCapabilities",
    "PostToolRequest",
    "PreToolAction",
    "PreToolCapabilities",
    "PreToolRequest",
    "RecoveryMethod",
    "ResultKind",
    "RetrieveRequest",
    "TokenlessConfig",
    "TokenlessError",
    "TokenlessSdk",
    "ToolResultStatus",
)

_SDKS: dict[str, Any] = {}


def _sdk_for(data_dir: str) -> Any:
    sdk = _SDKS.get(data_dir)
    if sdk is None:
        from anolisa_tokenless import TokenlessConfig, TokenlessSdk

        sdk = TokenlessSdk(
            TokenlessConfig(
                data_dir=data_dir, rtk_enabled=True, retrieve_tool_name=RETRIEVE_TOOL
            )
        )
        _SDKS[data_dir] = sdk
    return sdk


class TokenlessMiddleware(MiddlewareBase):
    """Apply the Tokenless lifecycle to one QwenPaw agent request."""

    def __init__(self, sdk: Any, data_dir: str) -> None:
        self.sdk = sdk
        self.data_dir = data_dir

    async def on_model_call(
        self,
        agent: Any,
        input_kwargs: dict[str, Any],
        next_handler: Callable[..., Any],
    ) -> Any:
        from anolisa_tokenless import (
            Attribution,
            BeforeModelCapabilities,
            BeforeModelRequest,
            RecoveryMethod,
        )

        tools = [
            tool
            for tool in input_kwargs["tools"]
            if tool.get("function", {}).get("name") != RETRIEVE_TOOL
        ]
        transformed = await self.sdk.before_model(
            BeforeModelRequest(
                tools=tuple(tools),
                visible_context=json.dumps(
                    input_kwargs["messages"], ensure_ascii=False, default=str
                ),
                capabilities=BeforeModelCapabilities(
                    replace_tools=True, recovery=RecoveryMethod.tool(RETRIEVE_TOOL)
                ),
                attribution=Attribution(AGENT_ID, agent.state.session_id),
            )
        )
        agent.state.middle_context[STATE_KEY] = {
            "visible_markers": sorted(transformed.visible_markers),
            "agent_id": AGENT_ID,
            "data_dir": self.data_dir,
        }
        tools = list(transformed.tools)
        tools.append(RETRIEVE_DECLARATION)
        return await next_handler(**{**input_kwargs, "tools": tools})

    async def on_acting(
        self,
        agent: Any,
        input_kwargs: dict[str, Any],
        next_handler: Callable[..., AsyncGenerator[Any, None]],
    ) -> AsyncGenerator[Any, None]:
        from anolisa_tokenless import (
            Attribution,
            OutputOptimization,
            PreToolAction,
            PreToolCapabilities,
            PreToolRequest,
        )

        source = input_kwargs["tool_call"]
        if source.name == RETRIEVE_TOOL:
            async for item in next_handler(**input_kwargs):
                yield item
            return

        origin, command_field = _CONTRACTS.get(source.name, _DEFAULT_CONTRACT)
        attribution = Attribution(AGENT_ID, agent.state.session_id, source.id)
        optimization = OutputOptimization.NONE
        forwarded = source
        if command_field is not None:
            arguments = json.loads(source.input)
            if not isinstance(arguments, dict):
                raise TypeError(f"{source.name} arguments must be a JSON object")
            transformed = await self.sdk.pre_tool(
                PreToolRequest(
                    tool_name=source.name,
                    arguments=arguments,
                    command_field=command_field,
                    capabilities=PreToolCapabilities(
                        replace_arguments=True, block_and_suggest=False
                    ),
                    attribution=attribution,
                )
            )
            if transformed.action is PreToolAction.BLOCK_AND_SUGGEST:
                raise RuntimeError(
                    "Core returned block_and_suggest without host capability"
                )
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
                    item, source.name, origin, optimization, attribution
                )
            else:
                yield item

    async def _after_response(
        self,
        response: ToolResponse,
        tool_name: str,
        origin: str,
        optimization: Any,
        attribution: Any,
    ) -> ToolResponse:
        from anolisa_tokenless import (
            ContentOrigin,
            OutputOptimization,
            PostToolCapabilities,
            PostToolRequest,
            RecoveryMethod,
            ResultKind,
            ToolResultStatus,
        )

        status = {
            ToolResultState.SUCCESS: ToolResultStatus.SUCCESS,
            ToolResultState.ERROR: ToolResultStatus.ERROR,
            ToolResultState.INTERRUPTED: ToolResultStatus.INTERRUPTED,
            ToolResultState.DENIED: ToolResultStatus.DENIED,
        }[response.state]
        # Recoverable compression needs the static retrieve tool; RTK output
        # and failed results stay lossless-only, as in tokenless_agentscope.
        recovery = (
            RecoveryMethod.tool(RETRIEVE_TOOL)
            if status is ToolResultStatus.SUCCESS
            and optimization == OutputOptimization.NONE
            else RecoveryMethod()
        )
        content = list(response.content)
        extra_context: str | None = None
        for index, block in enumerate(content):
            if not isinstance(block, TextBlock):
                continue
            transformed = await self.sdk.post_tool(
                PostToolRequest(
                    result_kind=ResultKind.TOOL,
                    tool_name=tool_name,
                    content=block.text,
                    status=status,
                    content_origin=ContentOrigin(origin),
                    output_optimization=optimization,
                    capabilities=PostToolCapabilities(
                        replace_output=True,
                        recovery=recovery,
                        replace_with_text=True,
                    ),
                    attribution=attribution,
                )
            )
            extra_context = extra_context or transformed.additional_context
            if transformed.output != block.text:
                content[index] = block.model_copy(update={"text": transformed.output})
        if extra_context is not None:
            content.append(TextBlock(text=extra_context))
        if content == response.content:
            return response
        return response.model_copy(update={"content": content})


def _factory(ctx: Any, agent_config: Any) -> TokenlessMiddleware:
    del agent_config
    # TokenlessConfig rejects relative data directories.
    data_dir = str(Path(ctx.workspace.workspace_dir).expanduser().resolve() / ".tokenless")
    return TokenlessMiddleware(_sdk_for(data_dir), data_dir)


async def tokenless_retrieve(hash_or_marker: str) -> ToolChunk:
    """Restore omitted content behind a visible Tokenless recovery instruction.

    Args:
        hash_or_marker: The 24-character hash from a visible recovery
            instruction; historical <<tokenless:HASH>> markers are accepted.
    """
    from anolisa_tokenless import Attribution, RetrieveRequest, TokenlessError
    from qwenpaw.config.context import get_current_agent_state

    state = get_current_agent_state()
    saved = getattr(state, "middle_context", {}).get(STATE_KEY) if state else None
    if not saved:
        return ToolChunk(
            content=[TextBlock(text="Tokenless has no marker state for this session")],
            state=ToolResultState.ERROR,
        )
    try:
        response = await _sdk_for(saved["data_dir"]).retrieve(
            RetrieveRequest(
                hash_or_marker,
                frozenset(saved["visible_markers"]),
                Attribution(saved["agent_id"], state.session_id),
            )
        )
    except TokenlessError as error:
        return ToolChunk(
            content=[TextBlock(text=str(error))], state=ToolResultState.ERROR
        )
    return ToolChunk(
        content=[TextBlock(text=response.payload)], state=ToolResultState.SUCCESS
    )


class TokenlessPlugin:
    """QwenPaw plugin entry point."""

    def register(self, api: Any) -> None:
        import anolisa_tokenless

        manifest = json.loads(
            (Path(__file__).parent / "plugin.json").read_text(encoding="utf-8")
        )
        missing = [
            name for name in _REQUIRED_SDK_NAMES if not hasattr(anolisa_tokenless, name)
        ]
        if missing:
            logger.error(
                "tokenless: anolisa_tokenless %s lacks %s required by plugin %s; "
                "install the anolisa_tokenless wheel from the tokenless/v%s release "
                "into QwenPaw's Python environment. Tokenless stays disabled.",
                anolisa_tokenless.__version__,
                ", ".join(missing),
                manifest["version"],
                manifest["version"],
            )
            return
        if anolisa_tokenless.__version__ != manifest["version"]:
            logger.warning(
                "tokenless: plugin %s runs against anolisa_tokenless %s",
                manifest["version"],
                anolisa_tokenless.__version__,
            )
        api.register_middleware(_factory, priority=100)
        api.register_tool(
            tool_name=RETRIEVE_TOOL,
            tool_func=tokenless_retrieve,
            description="Restore content behind a Tokenless marker",
            enabled=True,
            tool_type="internal",
        )


plugin = TokenlessPlugin()
