"""Public message consumers share completion state and preserve fail-stop on incomplete replies."""

import json
from pathlib import Path
from typing import Any, AsyncIterator
from unittest.mock import patch

from aw_claude import create_client
from claude_agent_sdk import ClaudeSDKClient
from claude_startup import assert_released, bounded, options, record_process


async def incomplete(root: Path, config: dict[str, Any], consumer: str, broken: bool = False) -> None:
    client = create_client(options(root, "eof-" + consumer))
    interrupts = []
    original = ClaudeSDKClient.interrupt

    async def interrupt(sdk_client: ClaudeSDKClient) -> None:
        interrupts.append(sdk_client)
        if broken:
            raise RuntimeError("injected EOF interrupt failure")
        await original(sdk_client)

    async def eof(sdk_client: ClaudeSDKClient) -> AsyncIterator[Any]:
        if False:
            yield None

    with patch.object(ClaudeSDKClient, "interrupt", interrupt):
        try:
            await bounded(client.connect())
            pid = record_process(client)
            await client.query("incomplete response")
            with patch.object(ClaudeSDKClient, "receive_messages", eof):
                messages = client.receive_messages() if consumer == "messages" else client.receive_response()
                try:
                    async for _ in messages:
                        pass
                except RuntimeError as error:
                    assert str(error) == "Claude SDK response ended without ResultMessage", error
                else:
                    raise AssertionError("EOF without a ResultMessage returned success")
            assert not client._aw_query_finished
        finally:
            await bounded(client.disconnect())
    assert interrupts == [client], interrupts
    assert not Path(f"/proc/{pid}").exists()
    assert_released(client, root, config)


async def eof_stop_failure(root: Path, config: dict[str, Any]) -> None:
    await incomplete(root, config, "messages", True)
    raise AssertionError("Unacknowledged EOF interruption did not stop the host")


async def run(root: Path, config: dict[str, Any], task: Any) -> None:
    cases = []

    async def unnecessary_interrupt(client: ClaudeSDKClient) -> None:
        raise RuntimeError("Completed response must not be interrupted")

    with patch.object(ClaudeSDKClient, "interrupt", unnecessary_interrupt):
        for index, consumer in enumerate(["response", "messages"]):
            await task(index, "allow", consumer)
            cases.append({"case": consumer + "-complete", "passed": True})
    for consumer in ["response", "messages"]:
        await incomplete(root, config, consumer)
        cases.append({"case": consumer + "-eof", "passed": True})
    (root / "result.json").write_text(json.dumps({"cases": cases, "passed": True}))
