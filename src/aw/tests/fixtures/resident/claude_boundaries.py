"""Pinned CLI stream interruption and JSON-number round trips through real AW Hooks."""

import asyncio
import importlib.util
import json
import threading
from http.server import ThreadingHTTPServer

from aw_claude import create_client, create_sdk_mcp_server
from claude_agent_sdk import ClaudeAgentOptions, HookMatcher, ResultMessage, tool


async def run(root, source):
    cases = []
    for name, number in (("interrupt-text-stream", None), ("interrupt-tool-stream", None),
                         ("number-integral-float", 1.0), ("number-exponent", 1e2),
                         ("number-large-integer", 9007199254740993)):
        directory = root / name
        directory.mkdir()
        spec = importlib.util.spec_from_file_location(name, source)
        fixture = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(fixture)
        fixture.ROOT, fixture.MODE = directory, "bash-compress"
        fixture.BASH = name.startswith("interrupt-") or name == "number-exponent"
        fixture.ORIGINAL = "boundary result"
        interrupting = name.startswith("interrupt-")
        partial, disconnected = asyncio.Event(), asyncio.Event()
        loop = asyncio.get_running_loop()
        observed, handled = [], []

        class Handler(fixture.Handler):
            def reply(self, data, kind):
                if kind != "text/event-stream":
                    return super().reply(data, kind)
                values = [json.loads(line[6:]) for line in data.decode().splitlines()
                          if line.startswith("data: ")]
                for value in values:
                    delta = value.get("delta", {})
                    if number is not None and delta.get("type") == "input_json_delta":
                        arguments = json.loads(delta["partial_json"])
                        arguments["timeout" if fixture.BASH else "value"] = number
                        delta["partial_json"] = json.dumps(arguments)
                if interrupting:
                    values = values[:3]
                    if name == "interrupt-text-stream":
                        values[1]["content_block"] = {"type": "text", "text": ""}
                        values[2]["delta"] = {"type": "text_delta", "text": "unfinished"}
                    data = "".join(f"data: {json.dumps(value)}\n\n" for value in values).encode()
                    self.send_response(200)
                    self.send_header("Content-Type", kind)
                    self.send_header("Content-Length", str(len(data) + 100))
                    self.end_headers()
                    self.wfile.write(data)
                    self.wfile.flush()
                    self.connection.settimeout(10)
                    try:
                        assert self.rfile.read(1) == b"", "relay sent unexpected request bytes"
                    except (ConnectionResetError, BrokenPipeError):
                        pass
                    finally:
                        loop.call_soon_threadsafe(disconnected.set)
                    return
                data = "".join(f"data: {json.dumps(value)}\n\n" for value in values).encode()
                super().reply(data, kind)

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        (directory / "endpoint.json").write_text(json.dumps({"port": server.server_port}))

        async def observe(payload, call_id, context):
            observed.append(payload)
            return {}

        @tool("touch", "Number round-trip fixture", {"value": float})
        async def business(arguments):
            handled.append(arguments)
            (directory / "effect").write_text("called")
            return {"content": [{"type": "text", "text": "numeric result"}]}

        client = create_client(ClaudeAgentOptions(
            cwd=str(directory), model="claude-sonnet-4-6", setting_sources=[], max_turns=4,
            tools=["Bash"] if fixture.BASH else [], allowed_tools=["Bash", "mcp__local__touch"],
            include_partial_messages=True,
            hooks={event: [HookMatcher(hooks=[observe])] for event in
                   ("PreToolUse", "PostToolUse", "PostToolUseFailure")},
            mcp_servers={} if fixture.BASH else {
                "local": create_sdk_mcp_server("numbers", tools=[business])},
            env={"ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_port}",
                 "ANTHROPIC_API_KEY": "local-fixture", "AW_TEST_MODE": "allow",
                 "AW_TEST_TENANT": name, "CLAUDE_CONFIG_DIR": str(directory / "profile")},
        ))
        terminals = []

        async def consume():
            async for message in client.receive_response():
                if getattr(message, "event", {}).get("type") == "content_block_delta":
                    partial.set()
                if isinstance(message, ResultMessage):
                    terminals.append(message)

        try:
            async with asyncio.timeout(45):
                async with client:
                    pid = client._transport.delegate._process.pid
                    (directory / "sdk-process.json").write_text(json.dumps({
                        "pid": pid, "relay_port": client._aw_relay.server.sockets[0].getsockname()[1]}))
                    await client.query("Use the fixture tool")
                    if interrupting:
                        receiving = asyncio.create_task(consume())
                        try:
                            await asyncio.wait_for(partial.wait(), 10)
                            await asyncio.wait_for(client.interrupt(), 5)
                            await asyncio.wait_for(receiving, 10)
                            await asyncio.wait_for(disconnected.wait(), 5)
                        finally:
                            if not receiving.done():
                                receiving.cancel()
                            await asyncio.gather(receiving, return_exceptions=True)
                        assert not observed and not (directory / "effect").exists()
                        assert not client._aw_relay.exchanges
                        interrupting = False
                        fixture.REQUESTS.clear()
                    else:
                        await consume()
                    assert not client._aw_bridge.failure
                    assert not client._aw_bridge.invocations.active
                    await client.query("Reuse this client for another tool call")
                    await consume()
                    assert len(terminals) == 2 and not terminals[-1].is_error
                    assert client._transport.delegate._process.pid == pid
                    assert not client._aw_bridge.failure
                    assert not client._aw_bridge.invocations.active
                    assert not client._aw_bridge.invocations.pending
                    assert (directory / "effect").exists()
                    if number is not None:
                        key = "timeout" if fixture.BASH else "value"
                        values = [payload["tool_input"][key] for payload in observed]
                        assert len(values) == 4 and all(type(value) is int for value in values)
                        assert values == [int(float(number))] * 4, values
                        if handled:
                            assert [value["value"] for value in handled] == [int(float(number))] * 2
                    (directory / "native-events.json").write_text(json.dumps(observed))
                    cases.append({"case": name, "passed": True})
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=3)
            assert not thread.is_alive()
    (root / "result.json").write_text(json.dumps({"tasks": len(cases), "cases": cases, "passed": True}))
