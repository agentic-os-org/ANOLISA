"""Run after independently starting AW with an sdk/application target named app."""

import asyncio
import os
from aw_sdk import Instance


async def main() -> None:
    capabilities = {
        "adapter": "sdk",
        "version": "aw-hook-source/v1",
        "entrypoint": "application",
        "events": {"tool.before": ["observe", "block"], "tool.after": ["observe"]},
    }
    instance = Instance(
        endpoint=os.environ["AW_SOCKET"],
        target="app",
        availability="required",
        capabilities=capabilities,
        install=lambda routes: all(
            hook["event"] in capabilities["events"] and not hook.get("replace_result")
            for hook in routes["hooks"]
        ),
    )
    try:
        await instance.connect()
        async with await instance.task() as task:
            invocation = {
                "session_id": "example",
                "call_id": "1",
                "name": "echo",
                "input": {"text": "hello"},
            }
            decision = await task.emit("tool.before", invocation)
            if decision["decision"] == "block":
                raise RuntimeError("AW denied this tool")
            result = invocation["input"]["text"]
            print(result)
            await task.emit("tool.after", {**invocation, "result": result})
    finally:
        await instance.close()


asyncio.run(main())
