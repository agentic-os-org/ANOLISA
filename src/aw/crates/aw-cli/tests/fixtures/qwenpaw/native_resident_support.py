"""Exercise pinned AgentScope dispatch and QwenPaw Coordinator without a model."""

import asyncio
import atexit
from contextlib import asynccontextmanager
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
from types import MethodType, SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[5]
OUTPUT = Path(os.environ.get("AW_TEST_OUTPUT", ROOT / "target/qwenpaw-resident"))
OUTPUT.mkdir(parents=True, exist_ok=True)
PROFILE = tempfile.TemporaryDirectory(prefix="native-profile-", dir=OUTPUT)
atexit.register(PROFILE.cleanup)
os.environ["QWENPAW_WORKING_DIR"] = PROFILE.name

from agentscope.agent import Agent
from agentscope.message import TextBlock, ToolCallBlock
from agentscope.tool import ToolChunk, ToolResponse
from qwenpaw.tool_calls import ToolCoordinator, ToolCoordinatorMiddleware
from qwenpaw.agents.react_agent import QwenPawAgent

ROOT = Path(__file__).resolve().parents[5]
NATIVE = ROOT / "crates/aw-adapter/native"
SPEC = importlib.util.spec_from_file_location(
    "aw_resident_test", NATIVE / "qwenpaw/plugin.py",
    submodule_search_locations=[str(NATIVE / "qwenpaw")],
)
PLUGIN = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = PLUGIN
SPEC.loader.exec_module(PLUGIN)
SCOPE_SPEC = importlib.util.spec_from_file_location(
    SPEC.name + ".aw_scope", NATIVE / "claude-sdk/aw_scope.py"
)
SCOPE = importlib.util.module_from_spec(SCOPE_SPEC)
sys.modules[SCOPE_SPEC.name] = SCOPE
SCOPE_SPEC.loader.exec_module(SCOPE)
from aw_resident_test.aw_native import bind_agent, bind_coordinator
from aw_resident_test.aw_private import PrivateExecution, execution_environment
from aw_resident_test.aw_resident import ResidentMiddleware, ResidentFailure


class Ledger:
    """In-memory transport double; native dispatch and generator code stay real."""

    def __init__(self, config):
        self.invocation_limit = 4
        self.active = {}
        self.closed = False
        self.environment = None
        self.callbacks = []

    def check(self):
        if self.closed or self.environment is None:
            raise RuntimeError("scope unavailable")

    async def open(self, cwd, environment):
        self.environment = dict(environment)

    async def register(self, sequence, call_id, name, arguments):
        self.check()
        assert call_id not in self.active
        self.active[call_id] = sequence

    async def finish(self, sequence, call_id):
        assert self.active.pop(call_id) == sequence

    async def close(self):
        assert not self.active
        self.closed = True
        if self.environment is not None:
            self.environment.clear()

    async def invoke_native(self, hook, payload):
        code, output = await self.invoke(hook, payload)
        return code, output, b""

    async def invoke(self, hook, payload):
        self.callbacks.append((hook["event"], payload))
        return 0, b""


def response(text="native result"):
    return ToolResponse(content=[TextBlock(text=text)])


class NativeFixture(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.patch = patch("aw_resident_test.aw_resident.Scope", Ledger)
        self.patch.start()
        self.addCleanup(self.patch.stop)

    def setup_agent(self, tool, *, hooks=None, context=None, coordinator=None, toolkit=None):
        coordinator = coordinator or ToolCoordinator()
        bind_coordinator(coordinator)
        middleware = ResidentMiddleware({"hooks": hooks or []}, PLUGIN.AwToolMiddleware,
                                        context or SimpleNamespace())
        async def available(name, groups):
            return SimpleNamespace(input_schema={})
        async def call_tool(tool_call, state):
            async for item in tool(tool_call):
                yield item
        agent = SimpleNamespace(
            toolkit=toolkit or SimpleNamespace(check_tool_available=available, call_tool=call_tool),
            _reply_middlewares=[middleware],
            _acting_middlewares=[middleware, ToolCoordinatorMiddleware(coordinator)],
            _acting_impl=tool,
            state=SimpleNamespace(session_id="session", context=[], tool_context=SimpleNamespace(activated_groups=[])),
            _request_context={"session_id": "session", "agent_id": "agent"},
        )
        agent.close = MethodType(QwenPawAgent.close, agent)
        agent.reply_stream = MethodType(Agent.reply_stream, agent)
        agent.reply = MethodType(Agent.reply, agent)
        agent._reply = MethodType(Agent._reply, agent)
        bind_agent(agent)
        return middleware, agent, coordinator

    async def request(self, middleware, agent, calls):
        async def reply():
            for call in calls:
                async for item in Agent._acting(agent, call):
                    yield item
        return [item async for item in middleware.on_reply(agent, {}, reply)]

    def call(self, identifier="native", value=1):
        return ToolCallBlock(id=identifier, name="tool", input=json.dumps({"value": value}))
