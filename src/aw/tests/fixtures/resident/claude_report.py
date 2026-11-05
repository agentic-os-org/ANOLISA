"""Native HTTP MCP and Bash report recovery through the resident SDK and real CLI."""

from contextlib import aclosing
from http.server import ThreadingHTTPServer
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import threading

from aw_claude import create_client
from claude_agent_sdk import ClaudeAgentOptions, HookMatcher, ResultMessage


async def run(root, source):
    cases = []
    executable = json.loads(Path(os.environ["AW_CLAUDE_CONFIG"]).read_text())["executable"]
    for tool in ("http", "http-structured", "bash"):
        for mode in ("allow", "tokenless-error", "after-error", "after-timeout", "after-invalid"):
            name = f"{tool}-{mode}"
            directory = root / name
            directory.mkdir()
            spec = importlib.util.spec_from_file_location(name, source)
            fixture = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(fixture)
            fixture.ROOT, fixture.BASH = directory, tool == "bash"
            fixture.MODE = "bash-compress" if fixture.BASH else "http-mcp-compress"
            fixture.ORIGINAL = json.dumps({"rows": [
                {"id": index, "description": "stable fixture data"} for index in range(80)
            ]}, indent=2)
            native = []

            async def observe(payload, call_id, context):
                native.append(payload)
                if payload["hook_event_name"] == "PreToolUse" and tool.startswith("http"):
                    (directory / f"policy-{payload['tool_input']['value']}").touch()
                return {}

            class Handler(fixture.Handler):
                def reply(self, data, kind):
                    if tool == "http-structured" and kind == "application/json":
                        response = json.loads(data)
                        if "content" in response.get("result", {}):
                            response["result"]["structuredContent"] = {
                                "marker": "structured fixture", **json.loads(fixture.ORIGINAL)}
                            data = json.dumps(response).encode()
                    super().reply(data, kind)

            server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
            thread = threading.Thread(target=server.serve_forever)
            thread.start()
            (directory / "endpoint.json").write_text(json.dumps({"port": server.server_port}))
            endpoint = f"http://127.0.0.1:{server.server_port}"
            options = ClaudeAgentOptions(
                model="claude-sonnet-4-6", cwd=str(directory), max_turns=2,
                tools=["Bash"] if fixture.BASH else [],
                allowed_tools=["Bash" if fixture.BASH else "mcp__local__touch"],
                mcp_servers={} if fixture.BASH else {"local": {
                    "type": "http", "url": endpoint + "/mcp",
                    "headers": {"X-Inspection-Ctx-Token": "fixture-tenant"},
                }},
                settings=json.dumps({"env": {"ANTHROPIC_BASE_URL": endpoint,
                    "ANTHROPIC_API_KEY": "local-fixture", "AW_NATIVE_CONFIG": "retained"}}),
                hooks={event: [HookMatcher(hooks=[observe])] for event in ("PreToolUse", "PostToolUse")},
                env={"ANTHROPIC_BASE_URL": "http://127.0.0.1:1", "ANTHROPIC_API_KEY": "unused",
                     "AW_TEST_MODE": mode, "AW_TEST_TENANT": name,
                     "CLAUDE_CONFIG_DIR": str(directory / "profile")},
            )
            client = create_client(options)
            try:
                async with client:
                    pid = client._transport.delegate._process.pid
                    (directory / "sdk-process.json").write_text(json.dumps({"pid": pid}))
                    assert client.options.settings == options.settings
                    assert client.options.mcp_servers == options.mcp_servers
                    for turn in range(2):
                        await client.query("Run the fixture tool")
                        async with aclosing(client.receive_response()) as stream:
                            messages = [message async for message in stream]
                        assert any(isinstance(message, ResultMessage) and not message.is_error for message in messages)
                        assert client._transport.delegate._process.pid == pid
                        assert client._aw_bridge.failure is None
                        ledger = client._aw_bridge.invocations
                        assert not ledger.active and not ledger.pending and not ledger.running
                        status = json.loads(subprocess.check_output(
                            [executable, "status", "--config", str(root / "aw.json")], timeout=5))["result"]
                        assert status["events"] == 0, status
                        assert all(status["resources"]["usage"][key] == 0
                                   for key in ("invocations", "correlations", "calls")), status
                    requests = [value for value in fixture.REQUESTS if "messages" in value]
                    assert len(requests) == 4, requests
                    after = [event for event in native if event["hook_event_name"] == "PostToolUse"]
                    assert len(after) == 2
                    for request, event in zip(requests[1::2], after):
                        results = [block for message in request["messages"]
                                   for block in message.get("content", [])
                                   if isinstance(block, dict) and block.get("type") == "tool_result"]
                        content = results[-1]["content"]
                        text = content if isinstance(content, str) else "".join(block.get("text", "") for block in content)
                        assert not results[-1].get("is_error", False)
                        original = event["tool_response"]
                        if fixture.BASH:
                            original = original["stdout"]
                        if isinstance(original, list):
                            original = "".join(block.get("text", "") for block in original)
                        assert isinstance(original, str), original
                        if tool == "http-structured":
                            assert "structured fixture" in original
                        if mode == "allow":
                            assert original not in text and len(text) < len(original), text
                        else:
                            assert original in text, (name, text)
                    assert len([event for event in native if event["hook_event_name"] == "PostToolUse"]) == 2
                cases.append({"case": name, "passed": True, "turns": 2})
            finally:
                (directory / "requests.json").write_text(json.dumps(fixture.REQUESTS))
                server.shutdown()
                server.server_close()
                thread.join(timeout=3)
                assert not thread.is_alive()
    (root / "result.json").write_text(json.dumps({"passed": True, "tasks": len(cases), "cases": cases}))
