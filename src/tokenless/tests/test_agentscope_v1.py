#!/usr/bin/env python3
"""Unit tests for the AgentScope 1.x lifecycle integration."""

from __future__ import annotations

import importlib
import sys
import types
import unittest
from dataclasses import dataclass, replace
from pathlib import Path

_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(_ROOT / "python" / "tokenless" / "python"))
sys.path.insert(0, str(_ROOT / "python" / "agentscope" / "src"))


class _TokenlessError(Exception):
    pass


@dataclass
class _CompressionResult:
    output: str
    applied: bool


class _Runtime:
    def __init__(self, data_dir=None, **_kwargs):
        self.data_dir = str(data_dir or "/tmp/tokenless-test")


@dataclass
class _Response:
    content: list[dict]
    metadata: dict | None = None
    is_last: bool = True
    is_interrupted: bool = False


@dataclass
class _Registered:
    original_func: object
    json_schema: dict
    postprocess_func: object | None = None


class _Toolkit:
    def __init__(self) -> None:
        self.tools = {}

    def register_tool_function(self, func, *args, **kwargs) -> None:
        del args
        schema = kwargs.get("json_schema") or {
            "type": "function",
            "function": {"name": func.__name__, "parameters": {}},
        }
        self.tools[schema["function"]["name"]] = _Registered(
            func, schema, kwargs.get("postprocess_func")
        )


def _install_stubs() -> None:
    native = types.ModuleType("anolisa_tokenless._native")
    native._StatsQuery = object
    native.CompressionResult = _CompressionResult
    native.TokenlessError = _TokenlessError
    native.TokenlessRuntime = _Runtime
    native.__version__ = "0.0.0-test"
    agentscope = types.ModuleType("agentscope")
    agentscope.__path__ = []
    agentscope.__version__ = "1.0.11"
    message = types.ModuleType("agentscope.message")
    message.TextBlock = lambda **kwargs: kwargs
    tool = types.ModuleType("agentscope.tool")
    tool.Toolkit = _Toolkit
    tool.ToolResponse = _Response
    sys.modules.update(
        {
            "anolisa_tokenless._native": native,
            "agentscope": agentscope,
            "agentscope.message": message,
            "agentscope.tool": tool,
        }
    )


_install_stubs()
core = importlib.import_module("anolisa_tokenless")
api = importlib.import_module("tokenless_agentscope")


class _Model:
    stream = False

    async def __call__(self, *args, **kwargs):
        return args, kwargs


class _Agent:
    def __init__(self, toolkit) -> None:
        self.name = "agent-1"
        self.toolkit = toolkit
        self.model = _Model()
        self.hooks = {}

    def register_instance_hook(self, kind, name, hook) -> None:
        self.hooks[(kind, name)] = hook


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


class AgentScopeV1Test(unittest.IsolatedAsyncioTestCase):
    def setUp(self) -> None:
        contracts = {
            name: api.ToolContract(core.ContentOrigin.API_RESPONSE)
            for name in ("large_result", "dynamic", "api")
        }
        self.integration = api.TokenlessAgentScope(
            api.TokenlessConfig(rtk_enabled=False),
            tool_contracts=contracts,
        )
        self.toolkit = self.integration.create_toolkit()

        async def large_result():
            return _Response([])

        self.toolkit.register_tool_function(large_result)
        self.agent = _Agent(self.toolkit)
        self.integration.install(self.agent, session_id="session-1")

    async def test_dynamic_registration_is_wrapped_and_attributed(self) -> None:
        async def dynamic():
            return _Response([])

        self.toolkit.register_tool_function(dynamic)
        registered = self.toolkit.tools["dynamic"]

        async def post_tool(request):
            self.assertEqual(request.attribution.agent_id, "agent-1")
            self.assertEqual(request.attribution.session_id, "session-1")
            self.assertEqual(request.attribution.tool_use_id, "call-1")
            self.assertEqual(request.content_origin, core.ContentOrigin.API_RESPONSE)
            return _post_response("short")

        self.integration.sdk.post_tool = post_tool
        response = await registered.postprocess_func(
            {"name": "dynamic", "id": "call-1", "input": {}},
            _Response([{"type": "text", "text": "long text " * 100}]),
        )
        self.assertEqual(response.content[0]["text"], "short")

    async def test_model_proxy_keeps_retrieve_tool_static_while_markers_change(
        self,
    ) -> None:
        marker = "0123456789abcdef01234567"
        marker_sets = iter((frozenset(), frozenset({marker})))

        async def before_model(request):
            self.assertNotIn(
                "tokenless_retrieve",
                [tool.get("function", {}).get("name") for tool in request.tools],
            )
            self.assertEqual(
                request.capabilities.recovery, core.RecoveryMethod.tool("tokenless_retrieve")
            )
            return core.BeforeModelResponse(
                tools=request.tools,
                visible_markers=next(marker_sets),
            )

        self.integration.sdk.before_model = before_model
        _, first = await self.agent.model(
            [],
            tools=[registered.json_schema for registered in self.toolkit.tools.values()],
        )
        self.assertEqual(self.toolkit.visible_markers, frozenset())
        _, second = await self.agent.model([], tools=first["tools"])
        self.assertEqual(self.toolkit.visible_markers, frozenset({marker}))
        self.assertEqual(first["tools"], second["tools"])
        self.assertEqual(
            second["tools"][-1]["function"]["parameters"]["required"],
            ["hash_or_marker"],
        )

    async def test_pre_acting_preserves_original_call(self) -> None:
        original = {"name": "api", "id": "call-2", "input": {"value": 1}}
        hook = self.agent.hooks[("pre_acting", "tokenless")]
        result = await hook(self.agent, {"tool_call": original})
        self.assertIsNot(result["tool_call"], original)
        self.assertEqual(original["input"], {"value": 1})

    async def test_retrieve_bypasses_pre_acting_contracts(self) -> None:
        original = {
            "name": "tokenless_retrieve",
            "id": "call-retrieve",
            "input": {"hash_or_marker": "0123456789abcdef01234567"},
        }
        kwargs = {"tool_call": original}
        hook = self.agent.hooks[("pre_acting", "tokenless")]

        self.assertIs(await hook(self.agent, kwargs), kwargs)
        self.assertNotIn("call-retrieve", self.toolkit.output_optimizations)

    async def test_rtk_state_survives_until_the_final_response(self) -> None:
        self.integration.config = replace(self.integration.config, rtk_enabled=True)

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

        self.integration.sdk.pre_tool = pre_tool
        self.integration.sdk.post_tool = post_tool
        original = {
            "name": "shell",
            "id": "call-rtk",
            "input": {"command": "grep needle file.txt"},
        }
        hook = self.agent.hooks[("pre_acting", "tokenless")]
        transformed = (await hook(self.agent, {"tool_call": original}))["tool_call"]

        self.assertEqual(original["input"]["command"], "grep needle file.txt")
        self.assertEqual(transformed["input"]["command"], "rtk grep needle file.txt")
        self.assertEqual(
            self.toolkit.output_optimizations["call-rtk"],
            core.OutputOptimization.RTK,
        )

        partial = _Response([{"type": "text", "text": "stream"}], is_last=False)
        self.assertIs(
            await self.integration._after_tool(self.toolkit, transformed, partial),
            partial,
        )
        self.assertIn("call-rtk", self.toolkit.output_optimizations)

        final = _Response([{"type": "text", "text": "optimized output"}])
        self.assertIs(
            await self.integration._after_tool(self.toolkit, transformed, final),
            final,
        )
        self.assertEqual(observed[0].output_optimization, core.OutputOptimization.RTK)
        self.assertNotIn("call-rtk", self.toolkit.output_optimizations)

    def test_unknown_custom_tool_requires_contract(self) -> None:
        async def unknown():
            return _Response([])

        with self.assertRaisesRegex(ValueError, "ToolContract"):
            self.toolkit.register_tool_function(unknown)

    def test_retrieve_name_collision_is_rejected(self) -> None:
        original = self.toolkit.tools["tokenless_retrieve"].original_func

        async def tokenless_retrieve():
            return _Response([])

        with self.assertRaisesRegex(ValueError, "reserved"):
            self.toolkit.register_tool_function(tokenless_retrieve)
        self.assertIs(
            self.toolkit.tools["tokenless_retrieve"].original_func,
            original,
        )

    def test_requires_tokenless_toolkit_and_explicit_session(self) -> None:
        other = api.TokenlessAgentScope(api.TokenlessConfig(rtk_enabled=False))
        with self.assertRaisesRegex(RuntimeError, "installed before use"):
            other._attribution()
        with self.assertRaisesRegex(ValueError, "session_id"):
            other.install(_Agent(other.create_toolkit()), session_id="")
        with self.assertRaisesRegex(TypeError, "create_toolkit"):
            other.install(_Agent(_Toolkit()), session_id="session")


if __name__ == "__main__":
    unittest.main()
