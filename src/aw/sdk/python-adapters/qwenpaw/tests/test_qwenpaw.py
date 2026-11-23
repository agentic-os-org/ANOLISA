"""Exercise packaged middleware ordering with dependency-free native DTO stand-ins."""

from copy import deepcopy
import importlib
import json
import sys
from types import ModuleType, SimpleNamespace
import unittest
from unittest.mock import patch


class Response:
    def __init__(self, **values: object) -> None:
        self.values = values

    def model_dump(self, **_: object) -> dict:
        return deepcopy(self.values)

    @classmethod
    def model_validate(cls, values: dict) -> "Response":
        return cls(**values)


class Middleware(unittest.IsolatedAsyncioTestCase):
    async def test_before_order_and_after_unwind_preserve_replacement_chain(self) -> None:
        modules = {}
        for name, values in {
            "agentscope": {},
            "agentscope.middleware": {"MiddlewareBase": object},
            "agentscope.tool": {"ToolResponse": Response, "ToolChunk": type("Chunk", (), {})},
            "agentscope.message": {
                "TextBlock": dict,
                "ToolResultState": SimpleNamespace(DENIED="denied"),
            },
        }.items():
            modules[name] = ModuleType(name)
            modules[name].__dict__.update(values)
        with patch.dict(sys.modules, modules):
            native = importlib.import_module("aw_sdk_qwenpaw._native.sdk")
            calls = []
            hooks = [
                {"event": event, "step": step, "on_error": "report"}
                for event in ("tool.before", "tool.after")
                for step in ("outer", "inner")
            ]

            original_hooks = deepcopy(hooks)

            async def invoke(hook: dict, payload: dict) -> tuple[int, bytes, bytes]:
                calls.append((hook["event"], hook["step"], deepcopy(payload)))
                if hook["event"] == "tool.before":
                    return 0, b"", b""
                text = payload["tool_response"]["content"][0]["text"]
                value = "redacted" if hook["step"] == "inner" else "wrapped:" + text
                return (
                    0,
                    json.dumps(
                        {"aw_replacement": {"state": "success", "content": [{"text": value}]}}
                    ).encode(),
                    b"",
                )

            task = SimpleNamespace(fallback=False, routes={"hooks": hooks}, invoke_native=invoke)
            acting = native.Acting(task)
            original = Response(state="success", content=[{"text": "sensitive"}])
            chunk = object()
            executions = []

            async def execute(**_: object):
                executions.append("native")
                yield chunk
                yield original

            agent = SimpleNamespace(state=SimpleNamespace(session_id="session"))
            call = Response(id="call", name="custom", input="{}")
            output = [item async for item in acting.run(agent, {"tool_call": call}, execute)]
            self.assertEqual(executions, ["native"])
            self.assertEqual(
                [(event, step) for event, step, _ in calls],
                [
                    ("tool.before", "outer"),
                    ("tool.before", "inner"),
                    ("tool.after", "inner"),
                    ("tool.after", "outer"),
                ],
            )
            self.assertEqual(calls[-1][2]["tool_response"]["content"], [{"text": "redacted"}])
            self.assertIs(output[0], chunk)
            self.assertEqual(output[1].model_dump()["content"], [{"text": "wrapped:redacted"}])
            self.assertEqual(original.model_dump()["content"], [{"text": "sensitive"}])
            self.assertEqual(task.routes["hooks"], original_hooks)


if __name__ == "__main__":
    unittest.main()
