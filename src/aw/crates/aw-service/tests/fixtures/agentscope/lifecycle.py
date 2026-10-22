"""Native SDK lifecycle worker and bounded policy fixtures; no hosted model."""

from __future__ import annotations

import asyncio
import json
import os
import sys
from pathlib import Path
from typing import Any

ROOT = Path(os.environ["AW_TEST_ROOT"])
TEXT = json.dumps({"values": list(range(100)), "unicode": "你好"}, indent=2)


def record(name: str, value: Any) -> None:
    with (ROOT / f"{name}.jsonl").open("a") as output:
        output.write(json.dumps(value) + "\n")


async def worker() -> None:
    from agentscope.agent import InjectionConfig
    from agentscope.message import DataBlock, TextBlock, ToolCallBlock, ToolResultState, UserMsg
    from agentscope.permission import PermissionBehavior, PermissionDecision
    from agentscope.state import AgentState
    from agentscope.tool import ToolBase, ToolChunk, Toolkit
    from aw_agentscope import AwBridge
    from worker import OfflineModel

    class Tool(ToolBase):
        name = "exec"
        description = "Record fixture execution without running shell commands"
        is_read_only = False
        is_concurrency_safe = True
        input_schema = {
            "type": "object",
            "properties": {key: {"type": "string"} for key in ["command", "cwd", "mode"]},
            "required": ["command", "cwd", "mode"],
        }

        async def check_permissions(self, tool_input: dict[str, Any], context: Any) -> Any:
            record("permissions", {"tool": self.name, **tool_input})
            return PermissionDecision(
                behavior=(
                    PermissionBehavior.DENY
                    if tool_input["mode"] == "native-deny"
                    else PermissionBehavior.ALLOW
                ),
                message="Native fixture permission",
            )

        async def call(self, command: str, cwd: str, mode: str) -> Any:
            record("executed", {"tool": self.name, "command": command, "cwd": cwd, "mode": mode})
            content = [TextBlock(text=TEXT)]
            if mode == "media":
                content.append(
                    DataBlock(
                        source={
                            "type": "url",
                            "url": "https://example.invalid/image.png",
                            "media_type": "image/png",
                        }
                    )
                )
            yield ToolChunk(
                id="native-result",
                content=content,
                state={
                    "error": ToolResultState.ERROR,
                    "interrupted": ToolResultState.INTERRUPTED,
                }.get(mode, ToolResultState.SUCCESS),
                metadata={"kept": [None, True, {"unicode": "你好"}]},
                is_last=True,
            )

    shell = Tool()
    api = Tool()
    api.name = "fixture_api"
    model = OfflineModel([])
    agent = AwBridge().create_agent(
        name="lifecycle",
        system_prompt="Run local fixture tools",
        model=model,
        toolkit=Toolkit(tools=[shell, api]),
        state=AgentState(session_id="lifecycle-session"),
        injection_config=InjectionConfig(inject_runtime_state=False),
    )
    results = []

    async def turn(name: str, arguments: dict[str, Any], call_id: str) -> dict[str, Any]:
        model.calls = [ToolCallBlock(id=call_id, name=name, input=json.dumps(arguments))]
        model.round = 0
        await asyncio.wait_for(agent.reply(UserMsg("user", "run fixture")), 15)
        context = agent.state.model_dump(mode="json")["context"]
        result = [b for msg in context for b in msg["content"] if b["type"] == "tool_result"][-1]
        results.append(result)
        return result

    original = {"command": "git status", "cwd": "/fixture cwd", "mode": "success"}
    denied = await turn("exec", original, "original")
    assert denied["state"] == "denied", denied
    reason = denied["output"][0]["text"]
    prefix = "[tokenless:rewrite] Retry this tool with arguments: "
    assert reason.startswith(prefix), reason
    retry = json.loads(reason[len(prefix) :])
    assert retry["cwd"] == original["cwd"] and retry["mode"] == original["mode"]
    assert "TOKENLESS_AGENT_ID=sdk " in retry["command"]
    owned = await turn("exec", retry, "fresh-retry")
    assert owned["state"] == "success" and owned["output"][0]["text"] == TEXT, owned
    security = await turn(
        "exec", {**retry, "command": retry["command"] + " --aw-security-deny"}, "security-retry"
    )
    assert security["state"] == "denied", security
    native = await turn("exec", {**retry, "mode": "native-deny"}, "permission-retry")
    assert native["state"] == "denied", native
    for mode in ["success", "error", "interrupted", "media"]:
        result = await turn("fixture_api", {**original, "mode": mode}, "api-" + mode)
        text = result["output"][0]["text"]
        assert json.loads(text) == json.loads(TEXT), result
        assert (len(text) < len(TEXT)) == (mode == "success"), result
        assert result["metadata"] == {"kept": [None, True, {"unicode": "你好"}]}, result
        if mode in {"error", "interrupted"}:
            assert result["state"] == mode, result
        if mode == "media":
            assert len(result["output"]) == 2, result
    print(json.dumps(results))


def main() -> None:
    mode = sys.argv[1]
    if mode == "worker":
        asyncio.run(worker())
    elif mode == "scan":
        command = sys.argv[sys.argv.index("--code") + 1]
        record("scanned", command)
        print(
            json.dumps(
                {"ok": True, "verdict": "deny" if "--aw-security-deny" in command else "pass"}
            )
        )
    else:
        request = json.load(sys.stdin)
        record(mode, request)
        if mode == "native":
            if request["tool_call"]["id"] == "api-success":
                response = request["tool_response"]
                response["content"][0]["text"] = "unauthorized native replacement"
                print(json.dumps({"aw_replacement": response}))
                return
            raise SystemExit(3)
        print("true" if mode in {"observe", "late"} else "invalid boolean")


if __name__ == "__main__":
    main()
