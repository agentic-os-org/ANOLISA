#!/usr/bin/python3
"""Synthetic App lifecycle for CLI contract tests; native adoption is tested separately."""

import asyncio
import importlib.util
import json
import os
from pathlib import Path
import sys
from types import ModuleType, SimpleNamespace

if sys.argv[1:] == ["--version"]:
    print(os.environ.get("FAKE_QWENPAW_VERSION", "QwenPaw, version 2.2.2b4"))
    raise SystemExit(0)

root = Path(os.environ["FAKE_ROOT"])
working = Path(os.environ["QWENPAW_WORKING_DIR"])
plugin_path = working / "plugins/aw-native/plugin.py"
if os.environ.get("FAKE_QWENPAW_PROFILE"):
    os.environ["PROFILE_SECRET"] = "qwenpaw-callback-profile-secret"
    os.environ.pop("REMOVED_BY_NATIVE_HOST", None)
if os.environ.get("FAKE_QWENPAW_EARLY_EXIT"):
    raise SystemExit(7)


class Block:
    def __init__(self, **values):
        self.values = values
        self.__dict__.update(values)

    def model_dump(self, mode):
        return self.values


class Response(Block):
    def __init__(self, state="success", content=None, **values):
        super().__init__(state=state, content=[value.values for value in (content or [])], **values)


for name, values in {
    "agentscope": {},
    "agentscope.message": {"TextBlock": Block, "ToolResultState": SimpleNamespace(DENIED="denied")},
    "agentscope.middleware": {"MiddlewareBase": object},
    "agentscope.tool": {"ToolResponse": Response},
}.items():
    module = ModuleType(name)
    module.__dict__.update(values)
    sys.modules[name] = module

spec = importlib.util.spec_from_file_location("fixture_plugin", plugin_path)
plugin = importlib.util.module_from_spec(spec)
spec.loader.exec_module(plugin)
plugin.version = lambda name: "2.2.2b4" if name == "qwenpaw" else "2.0.8"
factories, startup = [], []
plugin.plugin.register(SimpleNamespace(
    config={},
    register_middleware=lambda factory, **kw: factories.append(factory),
    register_startup_hook=lambda name, callback: startup.append(callback),
))
for callback in startup:
    callback()


async def run(identifier="call-1"):
    executed = False
    middlewares = [factory(None, None) for factory in factories]
    call = Block(id=identifier, name="arbitrary-tool", input=json.dumps({
        "nested": [True, None, {"text": "任意 ; $(touch NEVER)"}],
        "deny": os.environ.get("FAKE_QWENPAW_DENY") == "1",
    }))
    agent = SimpleNamespace(state=SimpleNamespace(session_id="native-session"))

    async def chain(index=0):
        nonlocal executed
        if index == len(middlewares):
            executed = True
            (root / "tool-executed").write_text("yes")
            yield Response(content=[Block(text="native result")])
        else:
            async def next_handler():
                async for result in chain(index + 1):
                    yield result

            async for result in middlewares[index].on_acting(agent, {"tool_call": call}, next_handler):
                yield result

    results = [item.model_dump(mode="json") async for item in chain()]
    return {"tool_executed": executed, "results": results}


async def main():
    if os.environ.get("FAKE_QWENPAW_PARALLEL"):
        return await asyncio.gather(run("call-one"), run("call-two"))
    return await run()


print(json.dumps(asyncio.run(main())))
