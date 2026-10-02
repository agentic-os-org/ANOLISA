#!/usr/bin/env python3
"""Unit tests for the QwenPaw plugin lifecycle (no QwenPaw, AgentScope or wheel)."""

from __future__ import annotations

import copy
import importlib
import importlib.util
import json
import shutil
import sys
import tempfile
import types
import unittest
from unittest import mock
from dataclasses import dataclass, field
from enum import StrEnum
from pathlib import Path

_ROOT = Path(__file__).resolve().parents[1]
_PLUGIN_SRC = _ROOT / "adapters" / "tokenless" / "qwenpaw"
sys.path.insert(0, str(_ROOT / "python" / "tokenless" / "python"))


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


def _model_copy(self, *, update=None, deep=False):
    result = copy.deepcopy(self) if deep else copy.copy(self)
    for key, value in (update or {}).items():
        setattr(result, key, value)
    return result


@dataclass
class _TextBlock:
    text: str

    model_copy = _model_copy


@dataclass
class _DataBlock:
    data: bytes


@dataclass
class _ToolResponse:
    content: list
    state: _ResultState = _ResultState.SUCCESS

    model_copy = _model_copy


@dataclass
class _ToolChunk:
    content: list
    state: _ResultState = _ResultState.RUNNING


class _MiddlewareBase:
    pass


@dataclass
class _Call:
    id: str
    name: str
    input: str

    model_copy = _model_copy


@dataclass
class _State:
    session_id: str
    middle_context: dict = field(default_factory=dict)


@dataclass
class _Agent:
    name: str
    state: _State


_CURRENT_STATE: list = [None]


def _install_stubs() -> None:
    native = types.ModuleType("anolisa_tokenless._native")
    native._StatsQuery = object
    native.CompressionResult = _CompressionResult
    native.TokenlessError = _NativeError
    native.TokenlessRuntime = _Runtime
    native.__version__ = "0.0.0-test"
    agentscope = types.ModuleType("agentscope")
    agentscope.__path__ = []
    message = types.ModuleType("agentscope.message")
    message.TextBlock = _TextBlock
    message.ToolResultState = _ResultState
    middleware = types.ModuleType("agentscope.middleware")
    middleware.MiddlewareBase = _MiddlewareBase
    tool = types.ModuleType("agentscope.tool")
    tool.ToolChunk = _ToolChunk
    tool.ToolResponse = _ToolResponse
    qwenpaw = types.ModuleType("qwenpaw")
    qwenpaw.__path__ = []
    config = types.ModuleType("qwenpaw.config")
    config.__path__ = []
    context = types.ModuleType("qwenpaw.config.context")
    context.get_current_agent_state = lambda: _CURRENT_STATE[0]
    sys.modules.update(
        {
            "anolisa_tokenless._native": native,
            "agentscope": agentscope,
            "agentscope.message": message,
            "agentscope.middleware": middleware,
            "agentscope.tool": tool,
            "qwenpaw": qwenpaw,
            "qwenpaw.config": config,
            "qwenpaw.config.context": context,
        }
    )


_install_stubs()
core = importlib.import_module("anolisa_tokenless")

# QwenPaw loads the installed bundle, whose plugin.json is stamped; stamp a
# sandbox copy so the source tree stays untouched.
_SANDBOX = Path(tempfile.mkdtemp(prefix="tokenless-qwenpaw-test-"))
shutil.copy(_PLUGIN_SRC / "plugin.py", _SANDBOX / "plugin.py")
(_SANDBOX / "plugin.json").write_text(
    (_PLUGIN_SRC / "plugin.json.in")
    .read_text(encoding="utf-8")
    .replace("@VERSION@", "0.0.0-test"),
    encoding="utf-8",
)
_spec = importlib.util.spec_from_file_location("qwenpaw_tokenless_plugin", _SANDBOX / "plugin.py")
plugin = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(plugin)

DATA_DIR = "/tmp/tokenless-qwenpaw-test-data"


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


class _FakeApi:
    def __init__(self) -> None:
        self.middlewares: list = []
        self.tools: list = []

    def register_middleware(self, factory, *, priority):
        self.middlewares.append((factory, priority))

    def register_tool(self, **kwargs):
        self.tools.append(kwargs)


class PluginTest(unittest.IsolatedAsyncioTestCase):
    def setUp(self) -> None:
        self.sdk = core.TokenlessSdk(core.TokenlessConfig(data_dir=DATA_DIR, rtk_enabled=False))
        plugin._SDKS.clear()
        plugin._SDKS[DATA_DIR] = self.sdk
        self.middleware = plugin.TokenlessMiddleware(self.sdk, DATA_DIR)
        self.agent = _Agent(name="Paw", state=_State(session_id="session-1"))
        _CURRENT_STATE[0] = None

        async def pre_tool(_request):
            raise AssertionError("pre_tool must not run for this tool")

        self.sdk.pre_tool = pre_tool

    def test_register_wires_middleware_and_retrieve_tool(self) -> None:
        api = _FakeApi()
        with self.assertNoLogs(plugin.logger, level="WARNING"):
            plugin.plugin.register(api)
        self.assertEqual(api.middlewares, [(plugin._factory, 100)])
        self.assertEqual(len(api.tools), 1)
        self.assertEqual(api.tools[0]["tool_name"], "tokenless_retrieve")
        self.assertIs(api.tools[0]["tool_func"], plugin.tokenless_retrieve)
        self.assertTrue(api.tools[0]["enabled"])
        self.assertEqual(api.tools[0]["tool_type"], "internal")

    def test_register_warns_on_wheel_version_mismatch(self) -> None:
        stamped = json.loads((_SANDBOX / "plugin.json").read_text(encoding="utf-8"))
        stamped["version"] = "9.9.9"
        (_SANDBOX / "plugin.json").write_text(json.dumps(stamped), encoding="utf-8")
        try:
            with self.assertLogs(plugin.logger, level="WARNING") as logs:
                plugin.plugin.register(_FakeApi())
        finally:
            stamped["version"] = "0.0.0-test"
            (_SANDBOX / "plugin.json").write_text(json.dumps(stamped), encoding="utf-8")
        self.assertIn("plugin 9.9.9 runs against anolisa_tokenless 0.0.0-test", logs.output[0])

    def test_factory_uses_workspace_data_dir_and_caches_sdk(self) -> None:
        ctx = types.SimpleNamespace(
            workspace=types.SimpleNamespace(workspace_dir="/tmp/tokenless-qwenpaw-test-data/..")
        )
        plugin._SDKS["/tmp/.tokenless"] = self.sdk
        middleware = plugin._factory(ctx, agent_config=None)
        self.assertIs(middleware.sdk, self.sdk)
        self.assertEqual(middleware.data_dir, "/tmp/.tokenless")

    def test_factory_resolves_relative_workspace_dirs(self) -> None:
        ctx = types.SimpleNamespace(workspace=types.SimpleNamespace(workspace_dir="relative/ws"))
        expected = str(Path("relative/ws").resolve() / ".tokenless")
        plugin._SDKS[expected] = self.sdk
        self.assertEqual(plugin._factory(ctx, agent_config=None).data_dir, expected)

    def test_sdk_is_configured_with_the_registered_retrieve_tool(self) -> None:
        fake_sdk = lambda config: types.SimpleNamespace(config=config)  # noqa: E731
        with mock.patch.object(core, "TokenlessSdk", side_effect=fake_sdk):
            sdk = plugin._sdk_for("/tmp/tokenless-qwenpaw-test-config")
        self.assertEqual(sdk.config.retrieve_tool_name, "tokenless_retrieve")
        self.assertTrue(sdk.config.rtk_enabled)

    def test_register_refuses_a_wheel_without_the_required_sdk_surface(self) -> None:
        api = _FakeApi()
        with mock.patch.dict(core.__dict__):
            del core.RecoveryMethod
            with self.assertLogs(plugin.logger, level="ERROR") as logs:
                plugin.plugin.register(api)
        self.assertIn("lacks RecoveryMethod", logs.output[0])
        self.assertEqual(api.middlewares, [])
        self.assertEqual(api.tools, [])

    async def test_model_call_compresses_schemas_and_records_markers(self) -> None:
        seen = {}

        async def before_model(request):
            seen["request"] = request
            return core.BeforeModelResponse(
                tools=({"type": "function", "function": {"name": "read_file", "description": "short"}},),
                visible_markers=frozenset({"<<tokenless:b>>", "<<tokenless:a>>"}),
            )

        self.sdk.before_model = before_model
        tools = [
            {"type": "function", "function": {"name": "read_file", "description": "long"}},
            plugin.RETRIEVE_DECLARATION,
        ]
        input_kwargs = {"messages": [{"role": "user", "content": "hi"}], "tools": tools, "tool_choice": "auto"}
        snapshot = copy.deepcopy(input_kwargs)

        async def next_handler(**kwargs):
            return kwargs

        result = await self.middleware.on_model_call(self.agent, input_kwargs, next_handler)

        self.assertEqual(seen["request"].tools, (tools[0],))
        self.assertEqual(seen["request"].attribution, core.Attribution("qwenpaw", "session-1"))
        self.assertEqual(seen["request"].capabilities.recovery, core.RecoveryMethod.tool("tokenless_retrieve"))
        self.assertEqual(json.loads(seen["request"].visible_context), snapshot["messages"])
        self.assertEqual(
            result["tools"],
            [
                {"type": "function", "function": {"name": "read_file", "description": "short"}},
                plugin.RETRIEVE_DECLARATION,
            ],
        )
        self.assertEqual(result["tool_choice"], "auto")
        self.assertEqual(input_kwargs, snapshot)
        self.assertEqual(
            self.agent.state.middle_context["anolisa_tokenless"],
            {
                "visible_markers": ["<<tokenless:a>>", "<<tokenless:b>>"],
                "agent_id": "qwenpaw",
                "data_dir": DATA_DIR,
            },
        )

    async def test_shell_call_is_rewritten_on_a_copy_and_result_replaced(self) -> None:
        seen = {}

        async def pre_tool(request):
            seen["pre"] = request
            return core.PreToolResponse(
                arguments={"command": "/rtk ls"},
                action=core.PreToolAction.REPLACE_ARGUMENTS,
                output_optimization=core.OutputOptimization.RTK,
            )

        async def post_tool(request):
            seen["post"] = request
            return _post_response("small")

        self.sdk.pre_tool = pre_tool
        self.sdk.post_tool = post_tool
        call = _Call(id="call-1", name="execute_shell_command", input='{"command": "ls"}')
        forwarded = {}

        async def next_handler(**kwargs):
            forwarded.update(kwargs)
            yield "stream-chunk"
            yield _ToolResponse(content=[_TextBlock(text="big output")])

        items = await _collect(self.middleware.on_acting(self.agent, {"tool_call": call}, next_handler))

        self.assertEqual(seen["pre"].tool_name, "execute_shell_command")
        self.assertEqual(seen["pre"].arguments, {"command": "ls"})
        self.assertEqual(seen["pre"].command_field, "command")
        self.assertEqual(seen["pre"].attribution, core.Attribution("qwenpaw", "session-1", "call-1"))
        self.assertTrue(seen["pre"].capabilities.replace_arguments)
        self.assertFalse(seen["pre"].capabilities.block_and_suggest)
        self.assertEqual(forwarded["tool_call"].input, '{"command":"/rtk ls"}')
        self.assertEqual(call.input, '{"command": "ls"}')
        self.assertEqual(seen["post"].content_origin, core.ContentOrigin.COMMAND_OUTPUT)
        self.assertEqual(seen["post"].output_optimization, core.OutputOptimization.RTK)
        self.assertEqual(seen["post"].capabilities.recovery, core.RecoveryMethod())
        self.assertEqual(seen["post"].status, core.ToolResultStatus.SUCCESS)
        self.assertEqual(seen["post"].result_kind, core.ResultKind.TOOL)
        self.assertEqual(seen["post"].tool_name, "execute_shell_command")
        self.assertEqual(items[0], "stream-chunk")
        self.assertEqual(items[1].content, [_TextBlock(text="small")])

    async def test_non_object_shell_arguments_are_rejected(self) -> None:
        call = _Call(id="call-1", name="execute_shell_command", input='["ls"]')

        async def next_handler(**_kwargs):
            yield _ToolResponse(content=[])

        with self.assertRaises(TypeError):
            await _collect(self.middleware.on_acting(self.agent, {"tool_call": call}, next_handler))

    async def test_block_and_suggest_is_rejected(self) -> None:
        async def pre_tool(_request):
            return core.PreToolResponse(
                arguments={}, action=core.PreToolAction.BLOCK_AND_SUGGEST,
                output_optimization=core.OutputOptimization.RTK,
            )

        self.sdk.pre_tool = pre_tool
        call = _Call(id="call-1", name="execute_shell_command", input='{"command": "ls"}')

        async def next_handler(**_kwargs):
            yield _ToolResponse(content=[])

        with self.assertRaises(RuntimeError):
            await _collect(self.middleware.on_acting(self.agent, {"tool_call": call}, next_handler))

    async def test_registered_origins_and_unregistered_tools_pass_through(self) -> None:
        origins = {}
        recoveries = set()

        async def post_tool(request):
            origins[request.tool_name] = request.content_origin
            recoveries.add(request.capabilities.recovery)
            return _post_response(request.content)

        self.sdk.post_tool = post_tool

        for name in ("read_file", "grep_search", "mcp__weather__lookup"):
            call = _Call(id=f"call-{name}", name=name, input="{}")

            async def next_handler(**kwargs):
                self.assertIs(kwargs["tool_call"], call)
                yield _ToolResponse(content=[_TextBlock(text="payload")])

            items = await _collect(self.middleware.on_acting(self.agent, {"tool_call": call}, next_handler))
            self.assertEqual(items[0].content, [_TextBlock(text="payload")])

        self.assertEqual(origins["read_file"], core.ContentOrigin.FILE_CONTENT)
        self.assertEqual(origins["grep_search"], core.ContentOrigin.API_RESPONSE)
        # Unregistered tools take the passthrough origin, never a compressible one.
        self.assertEqual(origins["mcp__weather__lookup"], core.ContentOrigin.FILE_CONTENT)
        self.assertEqual(recoveries, {core.RecoveryMethod.tool("tokenless_retrieve")})

    async def test_error_status_maps_and_additional_context_is_appended(self) -> None:
        seen = []

        async def post_tool(request):
            seen.append(request)
            return _post_response(request.content, additional_context="hint" if len(seen) == 1 else "ignored")

        self.sdk.post_tool = post_tool
        call = _Call(id="call-1", name="web_fetch", input="{}")
        original = _ToolResponse(
            content=[_TextBlock(text="one"), _DataBlock(data=b"x"), _TextBlock(text="two")],
            state=_ResultState.ERROR,
        )

        async def next_handler(**_kwargs):
            yield original

        items = await _collect(self.middleware.on_acting(self.agent, {"tool_call": call}, next_handler))

        self.assertEqual([request.status for request in seen], [core.ToolResultStatus.ERROR] * 2)
        self.assertEqual([request.capabilities.recovery for request in seen], [core.RecoveryMethod()] * 2)
        self.assertEqual(
            items[0].content,
            [_TextBlock(text="one"), _DataBlock(data=b"x"), _TextBlock(text="two"), _TextBlock(text="hint")],
        )
        self.assertEqual(items[0].state, _ResultState.ERROR)
        self.assertEqual(len(original.content), 3)

    async def test_unchanged_response_is_returned_as_is(self) -> None:
        async def post_tool(request):
            return _post_response(request.content)

        self.sdk.post_tool = post_tool
        call = _Call(id="call-1", name="web_fetch", input="{}")
        original = _ToolResponse(content=[_TextBlock(text="same")])

        async def next_handler(**_kwargs):
            yield original

        items = await _collect(self.middleware.on_acting(self.agent, {"tool_call": call}, next_handler))
        self.assertIs(items[0], original)

    async def test_retrieve_tool_call_bypasses_post_tool(self) -> None:
        async def post_tool(_request):
            raise AssertionError("retrieve results must not be compressed")

        self.sdk.post_tool = post_tool
        call = _Call(id="call-1", name="tokenless_retrieve", input='{"hash_or_marker": "abc"}')
        response = _ToolResponse(content=[_TextBlock(text="restored")])

        async def next_handler(**kwargs):
            self.assertIs(kwargs["tool_call"], call)
            yield response

        items = await _collect(self.middleware.on_acting(self.agent, {"tool_call": call}, next_handler))
        self.assertIs(items[0], response)

    async def test_retrieve_tool_uses_session_state(self) -> None:
        seen = {}

        async def retrieve(request):
            seen["request"] = request
            return core.RetrieveResponse(hash="abc", payload="full payload")

        self.sdk.retrieve = retrieve
        _CURRENT_STATE[0] = _State(
            session_id="session-1",
            middle_context={
                "anolisa_tokenless": {
                    "visible_markers": ["<<tokenless:abc>>"],
                    "agent_id": "qwenpaw",
                    "data_dir": DATA_DIR,
                }
            },
        )

        chunk = await plugin.tokenless_retrieve("<<tokenless:abc>>")

        self.assertEqual(
            seen["request"],
            core.RetrieveRequest(
                "<<tokenless:abc>>",
                frozenset({"<<tokenless:abc>>"}),
                core.Attribution("qwenpaw", "session-1"),
            ),
        )
        self.assertEqual(
            chunk,
            _ToolChunk(content=[_TextBlock(text="full payload")], state=_ResultState.SUCCESS),
        )

    async def test_retrieve_tool_reports_errors_as_tool_errors(self) -> None:
        async def retrieve(_request):
            raise core.TokenlessError("marker not authorized")

        self.sdk.retrieve = retrieve
        _CURRENT_STATE[0] = _State(
            session_id="session-1",
            middle_context={
                "anolisa_tokenless": {"visible_markers": [], "agent_id": "qwenpaw", "data_dir": DATA_DIR}
            },
        )
        chunk = await plugin.tokenless_retrieve("zzz")
        self.assertEqual(chunk.state, _ResultState.ERROR)
        self.assertEqual(chunk.content, [_TextBlock(text="marker not authorized")])

        _CURRENT_STATE[0] = _State(session_id="session-2")
        chunk = await plugin.tokenless_retrieve("zzz")
        self.assertEqual(chunk.state, _ResultState.ERROR)


if __name__ == "__main__":
    unittest.main()
