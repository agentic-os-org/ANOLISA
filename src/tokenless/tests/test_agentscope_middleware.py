#!/usr/bin/env python3
"""Unit tests for the AgentScope 2.x lifecycle middleware."""

from __future__ import annotations

import copy
import importlib
import json
import sys
import types
import unittest
from dataclasses import dataclass, field, replace
from enum import StrEnum
from pathlib import Path
from unittest import mock

_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(_ROOT / "python" / "tokenless" / "python"))
sys.path.insert(0, str(_ROOT / "python" / "agentscope" / "src"))


class _NativeError(Exception):
    pass


@dataclass
class _CompressionResult:
    output: str
    applied: bool


class _Runtime:
    def __init__(self, data_dir=None, **_kwargs):
        self.data_dir = str(data_dir or "/tmp/tokenless-test")


class _ResultState(StrEnum):
    RUNNING = "running"
    SUCCESS = "success"
    ERROR = "error"
    DENIED = "denied"
    INTERRUPTED = "interrupted"


@dataclass
class _TextBlock:
    text: str

    def model_copy(self, *, update=None, deep=False):
        result = copy.deepcopy(self) if deep else copy.copy(self)
        for key, value in (update or {}).items():
            setattr(result, key, value)
        return result


@dataclass
class _ToolResponse:
    content: list
    state: _ResultState = _ResultState.SUCCESS

    def model_copy(self, *, update=None, deep=False):
        result = copy.deepcopy(self) if deep else copy.copy(self)
        for key, value in (update or {}).items():
            setattr(result, key, value)
        return result


@dataclass
class _ToolChunk:
    content: list
    state: _ResultState = _ResultState.RUNNING


class _ToolBase:
    def __init__(self) -> None:
        pass

    async def call(self, **_kwargs):
        raise NotImplementedError


class _Toolkit:
    def __init__(self, tools=None):
        self.tools = {tool.name: tool for tool in tools or []}

    async def get_tool(self, name):
        return self.tools.get(name)

    async def add_tool(self, tool):
        self.tools[tool.name] = tool


class _MiddlewareBase:
    async def list_tools(self):
        return []


class _PermissionBehavior(StrEnum):
    ALLOW = "allow"


@dataclass
class _PermissionDecision:
    behavior: _PermissionBehavior
    message: str


@dataclass
class _Call:
    id: str
    name: str
    input: str

    def model_copy(self, *, update=None, deep=False):
        result = copy.deepcopy(self) if deep else copy.copy(self)
        for key, value in (update or {}).items():
            setattr(result, key, value)
        return result


@dataclass
class _State:
    session_id: str
    context: list = field(default_factory=list)
    summary: object = ""
    middle_context: dict = field(default_factory=dict)


@dataclass
class _Agent:
    name: str
    state: _State


def _install_stubs() -> None:
    native = types.ModuleType("anolisa_tokenless._native")
    native._StatsQuery = object
    native.CompressionResult = _CompressionResult
    native.TokenlessError = _NativeError
    native.TokenlessRuntime = _Runtime
    native.__version__ = "0.0.0-test"
    agentscope = types.ModuleType("agentscope")
    agentscope.__path__ = []
    agentscope.__version__ = "2.0.5"
    message = types.ModuleType("agentscope.message")
    message.TextBlock = _TextBlock
    message.ToolResultState = _ResultState
    middleware = types.ModuleType("agentscope.middleware")
    middleware.MiddlewareBase = _MiddlewareBase
    permission = types.ModuleType("agentscope.permission")
    permission.PermissionBehavior = _PermissionBehavior
    permission.PermissionDecision = _PermissionDecision
    tool = types.ModuleType("agentscope.tool")
    tool.ToolBase = _ToolBase
    tool.ToolChunk = _ToolChunk
    tool.ToolResponse = _ToolResponse
    tool.Toolkit = _Toolkit
    sys.modules.update(
        {
            "anolisa_tokenless._native": native,
            "agentscope": agentscope,
            "agentscope.message": message,
            "agentscope.middleware": middleware,
            "agentscope.permission": permission,
            "agentscope.tool": tool,
        }
    )


_install_stubs()
core = importlib.import_module("anolisa_tokenless")
api = importlib.import_module("tokenless_agentscope")


async def _collect(generator):
    return [item async for item in generator]


def _post_response(output: str, additional_context: str | None = None):
    return core.PostToolResponse(
        output=output,
        disposition=core.Disposition.PASSTHROUGH,
        content_type=core.ContentType.PLAIN_TEXT,
        applied_operations=(),
        recoverability=core.Recoverability.LOSSLESS,
        before_tokens=1,
        after_tokens=1,
        stash_keys=(),
        tokenizer_id="heuristic-v1",
        additional_context=additional_context,
    )


class MiddlewareTest(unittest.IsolatedAsyncioTestCase):
    def setUp(self) -> None:
        contracts = {
            name: api.ToolContract(core.ContentOrigin.API_RESPONSE)
            for name in ("api", "large_result")
        }
        self.middleware = api.TokenlessMiddleware(
            _config=api.TokenlessConfig(rtk_enabled=False),
            tool_contracts=contracts,
        )
        self.agent = _Agent("agent-2", _State("session-2"))

    async def test_model_call_keeps_tools_static_while_markers_change(self) -> None:
        marker = "0123456789abcdef01234567"
        marker_sets = iter((frozenset(), frozenset({marker})))

        async def before_model(request):
            self.assertEqual(request.tools[0]["function"]["name"], "api")
            self.assertEqual(request.tools[1], {"type": "web_search"})
            self.assertEqual(
                request.capabilities.recovery, core.RecoveryMethod.tool("tokenless_retrieve")
            )
            return core.BeforeModelResponse(
                tools=request.tools,
                visible_markers=next(marker_sets),
            )

        self.middleware.sdk.before_model = before_model
        observed = {}

        async def next_handler(**kwargs):
            observed.update(kwargs)
            return "model-response"

        input_kwargs = {
            "messages": [],
            "tools": [
                {
                    "type": "function",
                    "function": {"name": "api", "parameters": {}},
                },
                {"type": "web_search"},
                self.middleware._retrieve_declaration.as_function_tool(),
            ],
            "tool_choice": None,
            "current_model": object(),
        }
        result = await self.middleware.on_model_call(
            self.agent,
            input_kwargs,
            next_handler,
        )
        self.assertEqual(result, "model-response")
        first_tools = observed["tools"]
        self.assertEqual(first_tools[-1]["function"]["name"], "tokenless_retrieve")
        self.assertEqual(
            self.agent.state.middle_context["anolisa_tokenless"]["visible_markers"],
            [],
        )
        observed.clear()
        await self.middleware.on_model_call(
            self.agent,
            {**input_kwargs, "tools": first_tools},
            next_handler,
        )
        self.assertEqual(first_tools, observed["tools"])
        self.assertEqual(
            self.agent.state.middle_context["anolisa_tokenless"]["visible_markers"],
            [marker],
        )
        self.assertEqual(
            self.agent.state.middle_context["anolisa_tokenless"]["agent_id"],
            "agent-2",
        )

    async def test_acting_preserves_stream_and_transforms_final_text(self) -> None:
        observed = []

        async def post_tool(request):
            observed.append(request)
            return _post_response("short")

        self.middleware.sdk.post_tool = post_tool
        chunk = _ToolChunk([_TextBlock("stream")])
        response = _ToolResponse([_TextBlock("long " * 100)])

        async def next_handler(**kwargs):
            self.assertEqual(kwargs["tool_call"].input, "{}")
            yield chunk
            yield response

        output = await _collect(
            self.middleware.on_acting(
                self.agent, {"tool_call": _Call("call-1", "api", "{}")}, next_handler
            )
        )
        self.assertIs(output[0], chunk)
        self.assertEqual(output[1].content[0].text, "short")
        self.assertEqual(response.content[0].text, "long " * 100)
        self.assertEqual(observed[0].content_origin, core.ContentOrigin.API_RESPONSE)
        self.assertEqual(observed[0].attribution.tool_use_id, "call-1")

    async def test_rtk_state_reaches_the_final_response(self) -> None:
        self.middleware.config = replace(self.middleware.config, rtk_enabled=True)

        async def pre_tool(request):
            return core.PreToolResponse(
                arguments={
                    **request.arguments,
                    "command": "rtk grep needle file.txt",
                },
                action=core.PreToolAction.REPLACE_ARGUMENTS,
                output_optimization=core.OutputOptimization.RTK,
            )

        observed = []

        async def post_tool(request):
            observed.append(request)
            return _post_response(request.content)

        self.middleware.sdk.pre_tool = pre_tool
        self.middleware.sdk.post_tool = post_tool
        source = _Call(
            "call-rtk",
            "shell",
            json.dumps({"command": "grep needle file.txt"}),
        )
        chunk = _ToolChunk([_TextBlock("stream")])
        response = _ToolResponse([_TextBlock("optimized output")])

        async def next_handler(**kwargs):
            self.assertEqual(
                json.loads(kwargs["tool_call"].input)["command"],
                "rtk grep needle file.txt",
            )
            yield chunk
            yield response

        output = await _collect(
            self.middleware.on_acting(
                self.agent,
                {"tool_call": source},
                next_handler,
            )
        )

        self.assertEqual(json.loads(source.input)["command"], "grep needle file.txt")
        self.assertIs(output[0], chunk)
        self.assertIs(output[1], response)
        self.assertEqual(observed[0].output_optimization, core.OutputOptimization.RTK)

    async def test_error_context_is_owned_by_core(self) -> None:
        async def post_tool(request):
            self.assertEqual(request.status, core.ToolResultStatus.ERROR)
            return _post_response(
                request.content,
                "Environment diagnosis from Core.",
            )

        self.middleware.sdk.post_tool = post_tool
        response = _ToolResponse([_TextBlock("command not found")], state=_ResultState.ERROR)

        async def next_handler(**_kwargs):
            yield response

        output = await _collect(
            self.middleware.on_acting(
                self.agent, {"tool_call": _Call("call-2", "api", "{}")}, next_handler
            )
        )
        self.assertEqual(output[0].content[1].text, "Environment diagnosis from Core.")

    async def test_retrieve_uses_only_middleware_marker_state(self) -> None:
        marker = "0123456789abcdef01234567"
        self.agent.state.middle_context["anolisa_tokenless"] = {
            "visible_markers": [marker],
            "agent_id": "agent-2",
        }

        async def retrieve(request):
            self.assertEqual(request.hash_or_marker, marker.upper())
            self.assertEqual(request.visible_markers, frozenset({marker}))
            self.assertEqual(request.attribution.agent_id, "agent-2")
            return core.RetrieveResponse(hash=marker, payload="payload")

        self.middleware.sdk.retrieve = retrieve
        result = await self.middleware.retrieve_tool.call(marker.upper(), self.agent.state)
        self.assertEqual(result.content[0].text, "payload")

    async def test_retrieve_response_bypasses_post_tool(self) -> None:
        async def post_tool(_request):
            raise AssertionError("Retrieve output reached PostTool")

        self.middleware.sdk.post_tool = post_tool
        response = _ToolResponse([_TextBlock("restored payload")])

        async def next_handler(**_kwargs):
            yield response

        output = await _collect(
            self.middleware.on_acting(
                self.agent,
                {
                    "tool_call": _Call(
                        "call-retrieve",
                        "tokenless_retrieve",
                        json.dumps({"hash_or_marker": "0123456789abcdef01234567"}),
                    )
                },
                next_handler,
            )
        )
        self.assertIs(output[0], response)

    async def test_register_tools_rejects_collision(self) -> None:
        self.assertIn("Pass only the 24-character hash", self.middleware.retrieve_tool.description)
        self.assertIn("not the whole instruction", self.middleware.retrieve_tool.description)
        self.assertEqual(
            self.middleware.retrieve_tool.description,
            self.middleware._retrieve_declaration.description,
        )
        self.assertEqual(
            self.middleware.retrieve_tool.input_schema,
            self.middleware._retrieve_declaration.input_schema,
        )
        toolkit = _Toolkit()
        await self.middleware.register_tools(toolkit)
        self.assertIs(await toolkit.get_tool("tokenless_retrieve"), self.middleware.retrieve_tool)
        other = api.TokenlessMiddleware(_config=api.TokenlessConfig(rtk_enabled=False))
        with self.assertRaisesRegex(ValueError, "already contains"):
            await other.register_tools(toolkit)

    async def test_app_factory_retrieve_uses_middleware_marker_state(self) -> None:
        marker = "0123456789abcdef01234567"
        integration = api.TokenlessAgentScope(
            api.TokenlessConfig(data_dir="/tmp/tokenless-app", rtk_enabled=False),
            tool_contracts={
                "api": api.ToolContract(core.ContentOrigin.API_RESPONSE),
            },
        )
        options = integration.app_options()
        self.assertEqual(set(options), {"extra_agent_middlewares"})
        middleware = (await options["extra_agent_middlewares"]("user", "agent", "session"))[0]
        retrieve_tool = (await middleware.list_tools())[0]
        self.assertIs(retrieve_tool, middleware.retrieve_tool)

        state = types.SimpleNamespace(session_id="session", middle_context={})
        agent = types.SimpleNamespace(name="agent", state=state)

        async def before_model(request):
            return core.BeforeModelResponse(
                tools=request.tools,
                visible_markers=frozenset({marker}),
            )

        async def retrieve(request):
            self.assertEqual(request.visible_markers, frozenset({marker}))
            return core.RetrieveResponse(hash=marker, payload="payload")

        async def next_handler(**_kwargs):
            return "model-response"

        middleware.sdk.before_model = before_model
        middleware.sdk.retrieve = retrieve
        await middleware.on_model_call(
            agent,
            {"messages": [], "tools": []},
            next_handler,
        )
        result = await retrieve_tool.call(marker, state)
        self.assertEqual(result.content[0].text, "payload")

        other = (await options["extra_agent_middlewares"]("user", "agent", "other-session"))[0]
        self.assertNotEqual(other.data_dir, middleware.data_dir)

    def test_app_factory_requires_modern_tool_abi(self) -> None:
        integration = api.TokenlessAgentScope(
            api.TokenlessConfig(data_dir="/tmp/tokenless-app", rtk_enabled=False)
        )
        module = importlib.import_module("tokenless_agentscope._v2")

        with (
            mock.patch.object(module, "ToolBase", type("LegacyToolBase", (), {})),
            self.assertRaisesRegex(RuntimeError, "requires AgentScope 2.0.3"),
        ):
            integration.app_options()

    async def test_unknown_custom_tool_requires_contract(self) -> None:
        async def next_handler(**_kwargs):
            return "unreachable"

        with self.assertRaisesRegex(ValueError, "ToolContract"):
            await self.middleware.on_model_call(
                self.agent,
                {
                    "messages": [],
                    "tools": [
                        {
                            "type": "function",
                            "function": {"name": "custom", "parameters": {}},
                        }
                    ],
                },
                next_handler,
            )


if __name__ == "__main__":
    unittest.main()
