#!/usr/bin/env python3
"""Reusable native seam fixture; command files coordinate real AW CLI/RPC calls."""

import asyncio
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import time
from types import ModuleType, SimpleNamespace

adapter = os.environ["FAKE_RELOAD_ADAPTER"]
if sys.argv[1:] == ["--version"]:
    print("1.1.73" if adapter == "qoder" else "QwenPaw, version 2.2.2b4")
    raise SystemExit(0)
root = Path(os.environ["FAKE_ROOT"])
deadline = time.monotonic() + 10


def publish(path, text):
    # Readers treat file existence as completion.
    temporary = path.with_suffix(".tmp")
    temporary.write_text(text)
    temporary.replace(path)


async def wait(path):
    while time.monotonic() < deadline:
        if path.exists():
            return
        await asyncio.sleep(0.005)
    raise TimeoutError(f"fixture deadline: {path.name}")


if adapter == "qoder":
    settings = json.loads(Path(sys.argv[sys.argv.index("--settings") + 1]).read_text())

    def qoder_event(name, call="call", scenario="allow"):
        payload = {"hook_event_name": name, "cwd": str(root), "session_id": "session",
                   "tool_use_id": call, "tool_name": "custom_tool", "tool_input": {"x": 1},
                   "notification_type": "idle_prompt",
                   "scenario": scenario}
        if name == "PostToolUse":
            payload["tool_response"] = "native result"
        outputs = []
        for group in settings["hooks"].get(name, []):
            for hook in group["hooks"]:
                result = subprocess.run([hook["command"], *hook["args"]], input=json.dumps(payload).encode(),
                                        capture_output=True, timeout=6, check=False)
                outputs.append({"code": result.returncode, "stdout": result.stdout.decode(), "stderr": result.stderr.decode()})
        return outputs

else:
    class Block:
        def __init__(self, **values):
            self.values = values
            self.__dict__.update(values)

        def model_dump(self, mode):
            return self.values

    class Text(Block):
        def __init__(self, **values):
            super().__init__(type="text", **values)

    class Response(Block):
        def __init__(self, state="success", content=None, **values):
            super().__init__(state=state, content=[value.values for value in (content or [])], **values)

        @classmethod
        def model_validate(cls, value):
            response = cls()
            response.values = value
            response.__dict__.update(value)
            return response

    for name, values in {
        "agentscope.message": {"TextBlock": Text, "ToolResultState": SimpleNamespace(DENIED="denied")},
        "agentscope.middleware": {"MiddlewareBase": object},
        "agentscope.tool": {"ToolResponse": Response, "ToolChunk": type("ToolChunk", (), {})},
    }.items():
        module = ModuleType(name)
        module.__dict__.update(values)
        sys.modules[name] = module
    source = Path(os.environ["QWENPAW_WORKING_DIR"]) / "plugins/aw-native/plugin.py"
    spec = importlib.util.spec_from_file_location("reload_plugin", source)
    plugin = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(plugin)
    plugin.version = lambda name: "2.2.2b4" if name == "qwenpaw" else "2.0.9"
    factories, startup = [], []
    plugin.plugin.register(SimpleNamespace(config={}, register_middleware=lambda f, **kw: factories.append(f),
                                          register_startup_hook=lambda name, f: startup.append(f)))


async def main():
    if adapter == "qoder":
        assert all(item["code"] == 0 for item in qoder_event("SessionStart"))
    else:
        for callback in startup:
            await callback()
    publish(root / "native-ready", str(os.getpid()))
    for index in range(12):
        path = root / f"command-{index}.json"
        await wait(path)
        command = json.loads(path.read_text())
        if command["action"] == "stop":
            return
        if adapter == "qoder":
            started = qoder_event("UserPromptSubmit")
            assert all(item["code"] == 0 for item in started), started
            (root / f"entered-{index}").write_text("model wait")
            if command.get("hold"):
                await wait(root / f"continue-{index}")
            before = qoder_event("PreToolUse", f"call-{index}", command.get("scenario", "allow"))
            executed = not any(item["code"] == 2 for item in before)
            after = qoder_event("PostToolUse", f"call-{index}") if executed else []
            qoder_event("Notification")
            result = {"executed": executed, "before": before, "after": after}
        else:
            middleware = factories[0](None, None)
            agent = SimpleNamespace(state=SimpleNamespace(session_id="session"))
            executed = False

            async def tool(**kwargs):
                nonlocal executed
                executed = True
                yield Response(content=[Text(text="native result")], metadata={"kept": True})

            async def reply(**kwargs):
                (root / f"entered-{index}").write_text("model wait")
                if command.get("hold"):
                    await wait(root / f"continue-{index}")
                call = Block(id=f"call-{index}", name="custom_tool", input=json.dumps({"deny": command.get("scenario") == "block"}))
                async for item in middleware.on_acting(agent, {"tool_call": call}, tool):
                    yield item

            output = [item.model_dump(mode="json") async for item in middleware.on_reply(agent, {}, reply)]
            result = {"executed": executed, "output": output}
        publish(root / f"result-{index}.json", json.dumps({**result, "pid": os.getpid()}))
    raise RuntimeError("fixture command limit exceeded")


asyncio.run(main())
