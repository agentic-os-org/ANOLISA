"""Prove native cancellation drains asynchronous caller cleanup before reuse."""

import asyncio
import importlib.util
import json
import os
import threading
from http.server import ThreadingHTTPServer

from aw_claude import create_client
from claude_agent_sdk import ClaudeAgentOptions, HookMatcher, ResultMessage


async def run(root, source):
    directory = root / "drain"
    directory.mkdir()
    spec = importlib.util.spec_from_file_location("drain_model", source)
    fixture = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fixture)
    fixture.ROOT, fixture.MODE, fixture.BASH = directory, "bash-compress", True
    fixture.ORIGINAL = "drain result"
    server = ThreadingHTTPServer(("127.0.0.1", 0), fixture.Handler)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    (directory / "endpoint.json").write_text(json.dumps({"port": server.server_port}))
    entered, cleaning, release, draining = (asyncio.Event() for _ in range(4))
    completed = []

    async def hook(payload, call_id, context):
        if not entered.is_set():
            entered.set()
            try:
                await asyncio.Event().wait()
            finally:
                cleaning.set()
                await (
                    asyncio.Event()
                    if os.environ.get("AW_TEST_SCENARIO") == "drain-timeout"
                    else release
                ).wait()
        return {}

    options = ClaudeAgentOptions(
        cwd=str(directory),
        model="claude-sonnet-4-6",
        tools=["Bash"],
        allowed_tools=["Bash"],
        setting_sources=[],
        max_turns=4,
        hooks={"PostToolUse": [HookMatcher(hooks=[hook])]},
        env={
            "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_port}",
            "ANTHROPIC_API_KEY": "local-fixture",
            "AW_TEST_MODE": "allow",
            "AW_TEST_TENANT": "drain",
            "CLAUDE_CONFIG_DIR": str(directory / "profile"),
        },
    )
    client = create_client(options)
    try:
        async with asyncio.timeout(50):
            async with client:
                pid = client._transport.delegate._process.pid
                relay = client._aw_relay.server.sockets[0].getsockname()[1]
                (directory / "sdk-process.json").write_text(
                    json.dumps({"pid": pid, "relay_port": relay})
                )
                ledger = client._aw_bridge.invocations
                original = ledger.drain

                async def drain():
                    draining.set()
                    await original()

                ledger.drain = drain

                async def consume():
                    async for message in client.receive_response():
                        if isinstance(message, ResultMessage):
                            completed.append(message)

                await client.query("Run the fixture tool")
                receiving = asyncio.create_task(consume())
                try:
                    await asyncio.wait_for(entered.wait(), 10)
                    await asyncio.wait_for(client.interrupt(), 5)
                    await asyncio.wait_for(cleaning.wait(), 5)
                    await asyncio.wait_for(draining.wait(), 5)
                    assert not receiving.done() and not completed
                    assert ledger.pending and ledger.closing
                    if os.environ.get("AW_TEST_SCENARIO") == "drain-timeout":
                        (root / "drain-timeout-barrier").write_text("callback cleanup is held")
                        await receiving
                        raise AssertionError("undrained callback returned control to the host")
                    try:
                        await client.query("must wait for callback cleanup")
                    except RuntimeError:
                        pass
                    else:
                        raise AssertionError("new query crossed unfinished callback cleanup")
                finally:
                    release.set()
                    await asyncio.wait_for(receiving, 10)
                assert completed and not ledger.pending and not ledger.active

                # Keep the same native process/session after cancellation. This
                # also exercises the public AsyncIterable input contract.
                async def inputs():
                    yield {
                        "type": "user",
                        "message": {"role": "user", "content": "Run the tool again"},
                    }

                fixture.REQUESTS.clear()
                await client.query(inputs())
                await consume()
                assert len([row for row in fixture.REQUESTS if "messages" in row]) == 2
                assert len(completed) == 2 and not completed[-1].is_error
                assert client._transport.delegate._process.pid == pid
                assert not ledger.running and not ledger.pending and not ledger.active

                async def batch():
                    for index in range(2):
                        yield {
                            "type": "user",
                            "message": {"role": "user", "content": f"batch {index}"},
                        }

                requests = len(fixture.REQUESTS)
                try:
                    await client.query(batch())
                except ValueError as error:
                    assert "one user input message" in str(error)
                else:
                    raise AssertionError("ambiguous native input batch was admitted")
                assert len(fixture.REQUESTS) == requests
                await client.query("Run the tool after rejected input")
                await consume()
                assert len(completed) == 3
                assert not ledger.running and not ledger.pending and not ledger.active
                reader, writer = await asyncio.open_connection("127.0.0.1", relay)
                try:
                    # Keep disconnect in the SDK caller's task; Python 3.12
                    # must close an idle relay peer without its header timeout.
                    async with asyncio.timeout(3):
                        await client.disconnect()
                        assert await reader.read() == b""
                    assert not client._aw_relay.tasks
                finally:
                    writer.close()
                    await writer.wait_closed()
        (root / "result.json").write_text(
            json.dumps(
                {
                    "tasks": 1,
                    "passed": True,
                    "cases": [
                        "cancelled-callback-finally-drained",
                        "result-withheld-until-drain",
                        "query-rejected-during-drain",
                        "same-client-stream-input-reuse",
                        "multi-input-rejected-before-send",
                        "idle-relay-peer-does-not-delay-disconnect",
                    ],
                }
            )
        )
    finally:
        release.set()
        server.shutdown()
        server.server_close()
        thread.join(timeout=3)
        assert not thread.is_alive()
