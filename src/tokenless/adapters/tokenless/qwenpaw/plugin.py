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

import inspect
import json
import logging
import os
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

# Keyword arguments this plugin passes to each SDK constructor it calls. A
# released wheel that renames or drops one of them still imports and still
# exposes the class, so the name check alone passes and the middleware dies
# with a TypeError on the first model or tool call -- which is exactly how a
# wheel built before ``PostToolRequest.command`` reached users as a runtime
# crash instead of an install-time refusal.
_REQUIRED_SDK_FIELDS = {
    "BeforeModelCapabilities": ("recovery", "replace_tools"),
    "BeforeModelRequest": (
        "attribution",
        "capabilities",
        "tools",
        "visible_context",
    ),
    "PostToolCapabilities": ("recovery", "replace_output", "replace_with_text"),
    "PostToolRequest": (
        "attribution",
        "capabilities",
        "command",
        "content",
        "content_origin",
        "output_optimization",
        "result_kind",
        "status",
        "tool_name",
    ),
    "PreToolCapabilities": ("block_and_suggest", "replace_arguments"),
    "PreToolRequest": (
        "arguments",
        "attribution",
        "capabilities",
        "command_field",
        "tool_name",
    ),
    "TokenlessConfig": ("data_dir", "retrieve_tool_name", "rtk_enabled"),
}


def _missing_sdk_surface(sdk: Any) -> list[str]:
    """Members of ``sdk`` this plugin needs but the installed wheel lacks.

    Reports missing module attributes as bare names and constructor arguments
    this plugin cannot pass as ``Class.field``. Every field is handed over by
    keyword, so a parameter declared positional-only is unpassable and reported
    exactly like an absent one -- including when the same constructor also takes
    ``**kwargs``, which absorbs only the names it does not declare. A constructor
    whose signature cannot be introspected is left alone: this guard must never
    be the reason a compatible wheel is refused.
    """
    missing = [name for name in _REQUIRED_SDK_NAMES if not hasattr(sdk, name)]
    for class_name, fields in _REQUIRED_SDK_FIELDS.items():
        constructor = getattr(sdk, class_name, None)
        if constructor is None:
            continue  # already reported as a missing name
        try:
            parameters = inspect.signature(constructor).parameters
        except (TypeError, ValueError):
            continue
        # ``**kwargs`` covers only the names a signature does not declare, so it
        # is no excuse to skip the class: a field declared positional-only can
        # never receive the keyword this plugin passes, however permissive the
        # rest of the signature is.
        takes_any_keyword = any(
            parameter.kind is inspect.Parameter.VAR_KEYWORD
            for parameter in parameters.values()
        )
        positional_only = {
            name
            for name, parameter in parameters.items()
            if parameter.kind is inspect.Parameter.POSITIONAL_ONLY
        }
        missing.extend(
            f"{class_name}.{field}"
            for field in fields
            if field in positional_only
            or (field not in parameters and not takes_any_keyword)
        )
    return missing


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
        self.post_tool_failures = 0

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
        command = None
        if command_field is not None:
            try:
                arguments = json.loads(source.input)
                if not isinstance(arguments, dict):
                    raise TypeError(f"{source.name} arguments must be a JSON object")
            except (json.JSONDecodeError, TypeError):
                # The host (agentscope 2.x) repairs malformed tool
                # arguments upstream, but the raw string still reaches the
                # middleware; raising here kills the whole agent reply.
                # Fail open: forward the call untransformed and keep
                # lossless-only output handling.
                async for item in next_handler(**input_kwargs):
                    if isinstance(item, ToolResponse):
                        yield await self._after_response(
                            item,
                            source.name,
                            origin,
                            optimization,
                            attribution,
                            None,
                        )
                    else:
                        yield item
                return
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
            command = transformed.arguments.get(command_field)
        async for item in next_handler(**{**input_kwargs, "tool_call": forwarded}):
            if isinstance(item, ToolResponse):
                yield await self._after_response(
                    item,
                    source.name,
                    origin,
                    optimization,
                    attribution,
                    command if isinstance(command, str) else None,
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
        command: str | None,
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
            try:
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
                        command=command,
                    )
                )
            except Exception:
                # A compression failure must not turn a successful host tool
                # response into an agent error or partially rewrite its blocks.
                self.post_tool_failures += 1
                logger.exception(
                    "tokenless: post_tool failed for %s; preserving original result "
                    "(failures=%d)",
                    tool_name,
                    self.post_tool_failures,
                )
                return response
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
    configured = os.environ.get("TOKENLESS_DATA_DIR")
    if configured:
        configured_path = Path(configured).expanduser()
        if not configured_path.is_absolute():
            raise ValueError("TOKENLESS_DATA_DIR must be absolute")
        data_dir = str(configured_path.resolve())
    else:
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
        missing = _missing_sdk_surface(anolisa_tokenless)
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
