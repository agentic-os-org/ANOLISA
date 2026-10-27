#!/usr/bin/python3
"""Credential-free launcher wiring, with deterministic native framework doubles.

The staged AW plugin, builder guards, Scope CLI, service and Providers are real.
Native SDK streaming/cancellation semantics remain in the pinned native suites.
"""

import asyncio
from contextlib import nullcontext
from copy import deepcopy
from dataclasses import dataclass
import importlib.util
import inspect
import json
import os
from pathlib import Path
import sys
from types import ModuleType, SimpleNamespace

if sys.argv[1:] == ["--version"]:
    print("QwenPaw, version 2.2.2.post1")
    raise SystemExit(0)

ROOT = Path(os.environ["FAKE_ROOT"])
PLUGIN = Path(os.environ["QWENPAW_WORKING_DIR"]) / "plugins/aw-native/plugin.py"
FACTORIES, STARTUP = [], []


class Model:
    def __init__(self, **values):
        self.__dict__.update(values)

    def model_dump(self, mode="json"):
        return deepcopy(self.__dict__)

    @classmethod
    def model_validate(cls, values):
        return cls(**deepcopy(values))

    def model_copy(self, deep=False):
        return deepcopy(self)


class Response(Model):
    def __init__(self, state="success", content=None, **values):
        super().__init__(state=state, content=[
            value.model_dump() if isinstance(value, Model) else value
            for value in (content or [])
        ], **values)


class Chunk(Response):
    pass


class Stream:
    async def append(self, chunk):
        with (ROOT / "native-chunks.jsonl").open("a") as output:
            output.write(json.dumps(chunk.model_dump()) + "\n")

    async def close(self):
        pass


@dataclass
class Entry:
    ctx: object
    stream: object
    final_response: object
    end_state: str = "success"
    background_task: object = None


class Coordinator:
    def __init__(self):
        self._entries = {}
        self._completion_handlers = []
        self._offloaded_handlers = []
        self.hooks = SimpleNamespace(get=lambda name: None)

    def on_completion(self, callback):
        self._completion_handlers.append(callback)

    def on_offloaded(self, callback):
        self._offloaded_handlers.append(callback)

    async def execute(self, next_handler, tool_call):
        entry = Entry(SimpleNamespace(tool_call_id=tool_call.id), Stream(), Response())
        self._entries[tool_call.id] = entry
        try:
            await self._run_tool_with_hooks(next_handler, tool_call, entry)
            await self._finalize_completed(entry)
            yield entry.final_response
        finally:
            self._entries.pop(tool_call.id, None)

    async def _run_tool_with_hooks(self, next_handler, tool_call, entry):
        async for value in next_handler(tool_call=tool_call):
            if isinstance(value, Chunk):
                await entry.stream.append(value)
            else:
                entry.final_response = value

    async def _finalize_completed(self, entry):
        pass

    async def _supervise(self, entry, processor):
        raise AssertionError("background execution is outside this fixture")

    async def _begin_offload(self, entry, processor):
        raise AssertionError("offload is outside this fixture")

    async def shutdown(self):
        pass


class CoordinatorMiddleware:
    def __init__(self):
        self._coordinator = Coordinator()

    async def on_acting(self, agent, input_kwargs, next_handler):
        async for value in self._coordinator.execute(next_handler, input_kwargs["tool_call"]):
            yield value


class Toolkit:
    async def call_tool(self, tool_call, state):
        sys.modules["agentscope.tool._toolkit"]._json_loads_with_repair(tool_call.input, {})
        with (ROOT / "tool-executed").open("a") as output:
            output.write(tool_call.id + "\n")
        yield Chunk(content=[Model(text="protected tool output")])
        yield Response(content=[Model(text="protected tool output")])

    async def check_tool_available(self, name, groups):
        return SimpleNamespace(input_schema={})


class Agent:
    def __init__(self, middlewares):
        self._acting_middlewares = middlewares
        self._reply_middlewares = [mw for mw in middlewares if hasattr(mw, "on_reply")]
        self.state = SimpleNamespace(session_id="resident-session", tool_context=SimpleNamespace(activated_groups=[]))
        self.toolkit = Toolkit()

    async def _acting_impl(self, tool_call):
        async for value in self.toolkit.call_tool(tool_call, self.state):
            yield value

    async def _acting(self, tool_call):
        async def chain(index=0, **kwargs):
            if index == len(self._acting_middlewares):
                async for value in self._acting_impl(**kwargs):
                    yield value
            else:
                async def following(**next_kwargs):
                    async for value in chain(index + 1, **next_kwargs):
                        yield value
                async for value in self._acting_middlewares[index].on_acting(self, kwargs, following):
                    yield value
        async for value in chain(tool_call=tool_call):
            yield value

    async def reply_stream(self):
        async def dispatch():
            for index in range(2):
                call = Model(id=f"call-{index}", name="arbitrary-tool", input=json.dumps({
                    "deny": os.environ.get("FAKE_QWENPAW_DENY") == "1",
                }))
                async for value in self._acting(call):
                    yield value
        async for value in self._reply_middlewares[0].on_reply(self, {}, dispatch):
            yield value

    async def reply(self):
        return [value async for value in self.reply_stream()]

    async def close(self):
        pass


class AgentBuilder:
    @staticmethod
    def _build_middlewares(ctx, config):
        installed = [factory(ctx, config) for factory in FACTORIES]
        if os.environ.get("FAKE_QWENPAW_DROP_FACTORY"):
            installed = []
        return [CoordinatorMiddleware(), *installed]

    async def build(self):
        return Agent(self._build_middlewares(SimpleNamespace(workspace_dir=ROOT), None))


def parse_input(raw, schema=None):
    return json.loads(raw) if raw.strip() else {}


# These doubles expose only the installed native seam; no AW transport is mocked.
for name, values in {
    "agentscope": {},
    "agentscope.message": {"TextBlock": Model, "ToolResultState": SimpleNamespace(DENIED="denied")},
    "agentscope.middleware": {"MiddlewareBase": object},
    "agentscope.tool": {"ToolResponse": Response, "ToolChunk": Chunk},
    "agentscope.tool._toolkit": {"_json_loads_with_repair": parse_input},
    "agentscope._utils": {},
    "agentscope._utils._common": {"_json_loads_with_repair": parse_input},
    "qwenpaw": {},
    "qwenpaw.runtime": {},
    "qwenpaw.runtime.builder": {"AgentBuilder": AgentBuilder},
    "qwenpaw.tool_calls": {"ToolCoordinatorMiddleware": CoordinatorMiddleware},
    "anyio": {"CancelScope": lambda **kwargs: nullcontext()},
}.items():
    module = ModuleType(name)
    module.__dict__.update(values)
    sys.modules[name] = module

spec = importlib.util.spec_from_file_location("resident_fixture", PLUGIN, submodule_search_locations=[str(PLUGIN.parent)])
plugin = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = plugin
spec.loader.exec_module(plugin)
plugin.version = lambda name: "2.2.2.post1" if name == "qwenpaw" else "2.0.10"
plugin.plugin.register(SimpleNamespace(
    config={"enabled": not bool(os.environ.get("FAKE_QWENPAW_DISABLED"))},
    register_middleware=lambda factory, **kw: FACTORIES.append(factory),
    register_startup_hook=lambda name, callback: STARTUP.append(callback),
))
for callback in STARTUP:
    result = callback()
    if inspect.isawaitable(result):
        asyncio.run(result)
config = json.loads(Path(os.environ["AW_NATIVE_CONFIG"]).read_text())
assert config["resident"] is True
readiness = json.loads(Path(config["ready_path"]).read_text())


async def main():
    agent = await AgentBuilder().build()
    results, failure = [], None
    try:
        async for value in agent.reply_stream():
            results.append(value.model_dump())
    except RuntimeError as error:
        failure = type(error).__name__
    finally:
        await agent.close()
    return {"results": results, "failure": failure, "ready_hook_count": readiness["hooks"]}


print(json.dumps(asyncio.run(main())))
