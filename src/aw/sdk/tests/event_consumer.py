"""Installed Python SDK event drain, cancellation and reserved control capacity."""

import asyncio
import os
from pathlib import Path
import sys

from aw_sdk import AwError, Instance
from aw_sdk.transport import rpc


async def main() -> None:
    endpoint, root = sys.argv[1:]
    client = Instance(
        endpoint=endpoint,
        target="app",
        availability="required",
        timeout_ms=2000,
        capabilities={
            "adapter": "sdk",
            "version": "aw-hook-source/v1",
            "entrypoint": "application",
            "events": {"tool.before": ["observe", "block"]},
        },
        install=lambda _: True,
    )

    def native(call: str, **input: object) -> dict:
        return {
            "api_version": "aw-hook-source/v1",
            "event": "tool.before",
            "session_id": "session",
            "call_id": call,
            "name": "tool",
            "input": input,
        }

    async def probe(operation: dict) -> dict:
        return (await rpc(endpoint, client.connection["identity"], operation, 2000))["result"]

    try:
        await client.connect()
        task = await client.task()
        first, second = task.routes["hooks"]
        await task.invoke_native(first, native("partial"))
        assert (await probe({"method": "status"}))["resources"]["usage"]["event_slots"] == 1
        await task.close_event(first, native("partial"))
        assert (await probe({"method": "status"}))["resources"]["usage"]["event_slots"] == 0
        try:
            await task.invoke_native(second, native("partial"))
            raise AssertionError("closed event accepted")
        except AwError as error:
            assert error.code == "native_event_closed", error
        await task.emit("tool.before", native("complete"))
        async with asyncio.timeout(2):
            while (await probe({"method": "status"}))["resources"]["usage"]["event_slots"]:
                await asyncio.sleep(0.01)
        for call, explicit in (("cancelled", True), ("aborted", False)):
            (Path(root) / "event-pid").unlink(missing_ok=True)
            pending = asyncio.create_task(task.invoke_native(first, native(call, slow=True)))
            async with asyncio.timeout(5):
                while not (Path(root) / "event-pid").exists():
                    await asyncio.sleep(0.01)
            pid = int((Path(root) / "event-pid").read_text())
            assert (await probe({"method": "sdk_control", "command": {"action": "list"}}))[
                "applications"
            ] == []
            start = asyncio.get_running_loop().time()
            if explicit:
                await task.close_event(first, native(call, slow=True))
            pending.cancel()
            try:
                await pending
                raise AssertionError("cancellation swallowed")
            except asyncio.CancelledError:
                pass
            except AwError:
                assert explicit
            assert asyncio.get_running_loop().time() - start < 3
            try:
                os.kill(pid, 0)
                raise AssertionError("Provider still running after cancellation")
            except ProcessLookupError:
                pass
            assert (await probe({"method": "status"}))["resources"]["usage"]["event_slots"] == 0
        await task.emit("tool.before", native("fresh"))
        (Path(root) / "event-pid").unlink(missing_ok=True)
        pending = asyncio.create_task(task.invoke_native(first, native("scope-close", slow=True)))
        async with asyncio.timeout(5):
            while not (Path(root) / "event-pid").exists():
                await asyncio.sleep(0.01)
        pid = int((Path(root) / "event-pid").read_text())
        await task.close()
        try:
            await pending
            raise AssertionError("closed Scope completed a pending step")
        except AwError:
            pass
        try:
            os.kill(pid, 0)
            raise AssertionError("Provider survived Scope close")
        except ProcessLookupError:
            pass
        next_task = await client.task()
        await next_task.emit("tool.before", native("next-task"))
        await next_task.close()
    finally:
        await client.close()


asyncio.run(main())
