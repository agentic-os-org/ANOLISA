"""One real SDK client retains history through 1,280 mixed native/MCP call pairs."""

import asyncio
import json
import os
from pathlib import Path
import subprocess
import threading
import time
from http.server import ThreadingHTTPServer

from aw_claude import create_client, create_sdk_mcp_server
from claude_agent_sdk import ClaudeAgentOptions, HookMatcher, tool, ResultMessage
from long_model import Handler, ORIGINAL

ROOT = Path(os.environ["AW_TEST_ROOT"])


def process(pid):
    path = Path(f"/proc/{pid}")
    status = dict(line.split(":", 1) for line in (path / "status").read_text().splitlines())
    return {
        "pid": pid,
        "rss_kib": int(status["VmRSS"].split()[0]),
        "fds": len(list((path / "fd").iterdir())),
        "threads": int(status["Threads"]),
    }


async def main():
    directory = ROOT / "long"
    directory.mkdir()
    state = {"remaining": 0, "issued": 0, "requests": 0, "adopted": 0, "mcp": 0}
    Handler.state = state
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    (directory / "endpoint.json").write_text(
        json.dumps({"port": server.server_port, "pid": os.getpid()})
    )
    hooks = {"PreToolUse": 0, "PostToolUse": 0}
    sessions = set()

    async def hook(payload, call_id, context):
        hooks[payload["hook_event_name"]] += 1
        sessions.add(payload["session_id"])
        return {}

    @tool("touch", "Long session fixture", {"value": str})
    async def business(arguments):
        state["mcp"] += 1
        return {"content": [{"type": "text", "text": ORIGINAL}]}

    options = ClaudeAgentOptions(
        cwd=str(directory),
        model="claude-sonnet-4-6",
        tools=["Bash"],
        allowed_tools=["Bash", "mcp__local__touch"],
        setting_sources=[],
        max_turns=2000,
        mcp_servers={"local": create_sdk_mcp_server("long", tools=[business])},
        strict_mcp_config=True,
        hooks={name: [HookMatcher(hooks=[hook])] for name in hooks},
        env={
            "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_port}",
            "ANTHROPIC_API_KEY": "local-fixture",
            "CLAUDE_CONFIG_DIR": str(directory / "profile"),
            "AW_TEST_MODE": "allow",
            "AW_TEST_TENANT": "long",
        },
    )
    samples = []
    client = create_client(options)
    config = json.loads(Path(os.environ["AW_CLAUDE_CONFIG"]).read_text())
    started = time.monotonic()
    try:
        async with client:
            pid = client._transport.delegate._process.pid
            relay = client._aw_relay.server.sockets[0].getsockname()[1]
            (directory / "sdk-process.json").write_text(
                json.dumps({"pid": pid, "relay_port": relay})
            )
            for calls in [768, 128, 128, 128, 128]:
                state["remaining"] = calls
                await client.query("Execute the fixture sequence")
                terminal = False
                async for message in client.receive_response():
                    if isinstance(message, ResultMessage):
                        assert not message.is_error, message
                        terminal = True
                assert terminal and state["remaining"] == 0
                assert client._transport.delegate._process.pid == pid
                assert not client._aw_bridge.invocations.active
                assert not client._aw_bridge.invocations.pending
                status = json.loads(
                    subprocess.check_output(
                        [config["executable"], "status", "--config", str(ROOT / "aw.json")],
                        timeout=5,
                    )
                )["result"]
                assert status["resources"]["usage"]["invocations"] == 0
                assert status["resources"]["usage"]["correlations"] == 0
                samples.append(
                    {
                        "calls": state["issued"],
                        "seconds": time.monotonic() - started,
                        "aw": process(status["pid"]),
                        "worker": process(os.getpid()),
                        "sdk": process(pid),
                        "resources": status["resources"],
                    }
                )
        assert state["issued"] == 1280 and state["mcp"] == 640
        assert hooks == {"PreToolUse": 1280, "PostToolUse": 1280}
        assert len(sessions) == 1
        for name in ("aw", "worker"):
            assert samples[-1][name]["rss_kib"] - samples[0][name]["rss_kib"] < 65536
            assert samples[-1][name]["fds"] <= samples[0][name]["fds"] + 4
            assert samples[-1][name]["threads"] <= samples[0][name]["threads"] + 4
        (ROOT / "result.json").write_text(
            json.dumps(
                {
                    "tasks": 1,
                    "calls": 1280,
                    "executions": 5,
                    "mcp_calls": 640,
                    "native_calls": 640,
                    "samples": samples,
                    "state": state,
                    "passed": True,
                }
            )
        )
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=3)
        assert not thread.is_alive()


asyncio.run(main())
