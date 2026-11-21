"""Adopt semantic decisions using only the installed public Python package."""

import asyncio
import os
import sys
from aw_sdk import AwError, Instance


async def main() -> None:
    capabilities = {
        "adapter": "sdk",
        "version": "aw-hook-source/v1",
        "entrypoint": "application",
        "events": {
            "input.submit": ["observe", "block"],
            "tool.before": ["observe", "block"],
            "tool.after": ["observe"],
        },
    }
    instance = Instance(
        endpoint=sys.argv[1],
        target="app",
        availability="required",
        capabilities=capabilities,
        install=lambda routes: all(
            h["event"] in capabilities["events"] and not h.get("replace_result")
            for h in routes["hooks"]
        ),
    )
    try:
        assert await instance.connect()
        if os.environ.get("AW_SEMANTIC_EMPTY") == "1":
            async with await instance.task() as task:
                payload = {"session_id": "session", "call_id": "empty", "name": "bash", "input": {}}
                for event in ("tool.before", "tool.after"):
                    assert await task.emit(
                        event, {**payload, **({"result": "done"} if event == "tool.after" else {})}
                    ) == {"decision": "continue", "coverage": "checked"}
            return
        async with await instance.task() as task:
            assert await task.emit(
                "input.submit", {"session_id": "session", "turn_id": "turn", "text": "deny"}
            ) == {"decision": "block", "coverage": "checked"}
            executions = 0
            for command in ("deny", "allow"):
                payload = {
                    "session_id": "session",
                    "call_id": command,
                    "name": "bash",
                    "input": {"command": command},
                }
                reply = await task.emit("tool.before", payload)
                assert reply["coverage"] == "checked"
                if reply["decision"] == "continue":
                    executions += 1
                    assert await task.emit("tool.after", {**payload, "result": "done"}) == {
                        "decision": "continue",
                        "coverage": "checked",
                    }
            assert executions == 1
            for event, payload, code in [
                (
                    "tool.before",
                    {
                        "session_id": "session",
                        "call_id": "bad",
                        "name": "bash",
                        "input": {},
                        "instance_id": "foreign",
                    },
                    "sdk_input_invalid",
                ),
                ("agent.end", {}, "semantic_event_unavailable"),
            ]:
                try:
                    await task.emit(event, payload)
                except AwError as error:
                    assert error.code == code
                else:
                    raise AssertionError("invalid callback accepted")
    finally:
        await instance.close()
    capabilities["events"]["tool.after"].append("block")
    inflated = Instance(
        endpoint=sys.argv[1],
        target="app",
        availability="required",
        capabilities=capabilities,
        install=lambda _: True,
    )
    try:
        try:
            await inflated.connect()
        except AwError:
            pass
        else:
            raise AssertionError("unsupported effect accepted")
    finally:
        await inflated.close()


asyncio.run(main())
