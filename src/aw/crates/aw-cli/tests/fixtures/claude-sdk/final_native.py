"""Observe late native Post replacement at the real bundled CLI HTTP boundary."""

import asyncio
from dataclasses import asdict
import hashlib
from http.server import ThreadingHTTPServer
from importlib.metadata import version
import json
import os
from pathlib import Path
import platform
import subprocess
import threading
import time
from unittest.mock import patch

import claude_agent_sdk
from claude_agent_sdk import ClaudeAgentOptions, HookMatcher, create_sdk_mcp_server
from aw_claude import Bridge, create_client
import native_worker as fixture

ROOT = Path.cwd()
MARKER = "AW_CLAUDE_FINAL_SECRET_MARKER"
MODE = os.environ["CLAUDE_FINAL_CASE"]
BASH = MODE.startswith("bash-")
LATE = MODE.endswith("late")
TRACE = []


class Model(fixture.Handler):
    def completion_text(self):
        # Echo only content actually received in the second HTTP request.
        results = [block for message in fixture.REQUESTS[-1]["messages"]
                   for block in message.get("content", [])
                   if isinstance(block, dict) and block.get("type") == "tool_result"]
        return MARKER if MARKER in json.dumps(results) else "PUBLIC_OK"


def fingerprint():
    root = Path(claude_agent_sdk.__file__).parent
    return {str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in [root / "_bundled/claude", root / "_internal/query.py",
                         root / "_internal/transport/subprocess_cli.py", root / "types.py"]}


async def run(server):
    fixture.BASH = BASH
    fixture.ORIGINAL = "PUBLIC_TOOL_RESULT"
    completed = asyncio.Event()
    callback = Bridge.callback

    async def recorded(self, payload, call_id, context):
        result = await callback(self, payload, call_id, context)
        if payload["hook_event_name"] == "PostToolUse":
            TRACE.append({"phase": "aw_post_returned", "call_id": call_id,
                          "input": payload, "output": result})
            completed.set()
        return result

    async def late(payload, call_id, context):
        # SDK callbacks run concurrently. Release the application Hook only
        # after the real AW callback has returned; do not rely on a sleep.
        await asyncio.wait_for(completed.wait(), timeout=10)
        value = ({**payload["tool_response"], "stdout": MARKER} if BASH else MARKER)
        result = {"hookSpecificOutput": {"hookEventName": "PostToolUse",
                                         "updatedToolOutput": value}} if LATE else {}
        TRACE.append({"phase": "application_post_returned", "call_id": call_id,
                      "input": payload, "output": result})
        return result

    options = ClaudeAgentOptions(
        model="claude-sonnet-4-6", cwd=str(ROOT), setting_sources=[],
        tools=["Bash"] if BASH else [],
        allowed_tools=["Bash" if BASH else "mcp__local__touch"],
        mcp_servers={"local": create_sdk_mcp_server("local", tools=[fixture.touch])},
        hooks={"PostToolUse": [HookMatcher(hooks=[late], timeout=15)]},
        env={"ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_port}",
             "ANTHROPIC_API_KEY": "local-fixture",
             "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1"},
        max_turns=2,
    )
    messages = []
    baseline = fingerprint()
    start = time.monotonic()
    with patch.object(Bridge, "callback", recorded):
        async with create_client(options) as client:
            pid = client._transport._process.pid
            (ROOT / "cli-process.json").write_text(json.dumps({"pid": pid}))
            await client.query("Use the fixture tool once and report its result.")
            async for message in client.receive_response():
                messages.append({"type": type(message).__name__, "value": asdict(message)})
    elapsed = (time.monotonic() - start) * 1000
    assert not Path(f"/proc/{pid}").exists(), pid
    assert baseline == fingerprint(), "installed SDK/CLI changed"
    requests = [request for request in fixture.REQUESTS if "messages" in request]
    (ROOT / "requests.json").write_text(json.dumps(requests, indent=2))
    (ROOT / "messages.json").write_text(json.dumps(messages, indent=2))
    (ROOT / "trace.json").write_text(json.dumps(TRACE, indent=2))
    assert len(requests) == 2, len(requests)
    assert [row["phase"] for row in TRACE] == ["aw_post_returned", "application_post_returned"]
    assert TRACE[0]["call_id"] == TRACE[1]["call_id"]
    assert MARKER not in json.dumps(TRACE[0]), TRACE[0]
    results = [block for message in requests[1]["messages"]
               for block in message.get("content", [])
               if isinstance(block, dict) and block.get("type") == "tool_result"]
    assert len(results) == 1 and results[0]["tool_use_id"] == TRACE[0]["call_id"], results
    assert (MARKER in json.dumps(results)) == LATE, results
    result_messages = [message["value"] for message in messages if message["type"] == "ResultMessage"]
    assert len(result_messages) == 1 and not result_messages[0]["is_error"], result_messages
    assert result_messages[0]["result"] == (MARKER if LATE else "PUBLIC_OK"), result_messages
    assert (ROOT / "effect").exists()
    bundled = Path(claude_agent_sdk.__file__).parent / "_bundled/claude"
    (ROOT / "result.json").write_text(json.dumps({
        "case": MODE, "passed": True, "sdk": version("claude-agent-sdk"),
        "cli": subprocess.check_output([str(bundled), "--version"], text=True, timeout=5).strip(),
        "platform": platform.machine(), "python": platform.python_version(),
        "sha256": baseline, "model_requests": len(requests), "model_received_marker": LATE,
        "model_response": result_messages[0]["result"], "model_kind": "deterministic_local_http",
        "aw_post_before_application": True, "installed_files_unchanged": True,
        "tool_side_effect": True, "sdk_process_stopped": True, "elapsed_ms": elapsed,
        "final_gate": "unsupported",
    }, indent=2))


def main():
    server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    (ROOT / "server.json").write_text(json.dumps({"port": server.server_port}))
    try:
        asyncio.run(run(server))
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        assert not thread.is_alive()


if __name__ == "__main__":
    main()
