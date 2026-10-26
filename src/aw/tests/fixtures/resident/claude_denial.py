"""Native Hook and permission denials remain reusable executions without tool effects."""

import importlib.util
import json
import threading
from http.server import ThreadingHTTPServer

from aw_claude import create_client, create_sdk_mcp_server
from claude_agent_sdk import (
    ClaudeAgentOptions,
    HookMatcher,
    PermissionResultAllow,
    PermissionResultDeny,
    ResultMessage,
    tool,
)


async def run(root, source):
    cases = []
    for name in ("native-hook-deny", "permission-deny", "mcp-native-deny",
                 "native-input-validation", "native-permission-rule"):
        directory = root / name
        directory.mkdir()
        spec = importlib.util.spec_from_file_location(name, source)
        fixture = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(fixture)
        fixture.ROOT, fixture.MODE = directory, "bash-compress"
        fixture.BASH = name != "mcp-native-deny"
        fixture.ORIGINAL = "native denial result"
        server = ThreadingHTTPServer(("127.0.0.1", 0), fixture.Handler)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        (directory / "endpoint.json").write_text(json.dumps({"port": server.server_port}))
        denied = True
        permissions = []
        events = []

        class Handler(fixture.Handler):
            def reply(self, data, kind):
                if name == "native-input-validation" and denied and kind == "text/event-stream":
                    frames = []
                    for line in data.decode().splitlines():
                        if line.startswith("data: "):
                            value = json.loads(line[6:])
                            if value.get("delta", {}).get("type") == "input_json_delta":
                                value["delta"]["partial_json"] = '{"command":42}'
                            line = "data: " + json.dumps(value)
                        frames.append(line)
                    data = ("\n".join(frames) + "\n").encode()
                super().reply(data, kind)

        server.RequestHandlerClass = Handler

        async def observe(payload, call_id, context):
            events.append((denied, payload["hook_event_name"]))
            return {}

        async def hook(payload, call_id, context):
            if denied and name not in {"native-input-validation", "native-permission-rule"}:
                return {
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "deny",
                        "permissionDecisionReason": "native fixture",
                    }
                }
            return {}

        async def permission(tool_name, arguments, context):
            permissions.append(denied)
            return (
                PermissionResultDeny(message="native fixture")
                if denied
                else PermissionResultAllow()
            )

        @tool("touch", "Native permission fixture", {"value": str})
        async def business(arguments):
            (directory / "effect").write_text("called")
            return {"content": [{"type": "text", "text": "allowed result"}]}

        settings = ClaudeAgentOptions(
            cwd=str(directory),
            model="claude-sonnet-4-6",
            tools=["Bash"] if fixture.BASH else [],
            setting_sources=[],
            max_turns=4,
            allowed_tools=[] if name == "permission-deny" else ["Bash", "mcp__local__touch"],
            disallowed_tools=["Bash(printf *)"] if name == "native-permission-rule" else [],
            can_use_tool=permission if name == "permission-deny" else None,
            hooks={
                "PreToolUse": [HookMatcher(hooks=[observe, hook])],
                "PostToolUse": [HookMatcher(hooks=[observe])],
                "PostToolUseFailure": [HookMatcher(hooks=[observe])],
            } if name != "permission-deny" else {},
            mcp_servers=(
                {} if fixture.BASH else {"local": create_sdk_mcp_server("denial", tools=[business])}
            ),
            env={
                "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_port}",
                "ANTHROPIC_API_KEY": "local-fixture",
                "AW_TEST_MODE": "allow",
                "AW_TEST_TENANT": name,
                "CLAUDE_CONFIG_DIR": str(directory / "profile"),
            },
        )
        client = create_client(settings)
        try:
            async with client:
                (directory / "sdk-process.json").write_text(
                    json.dumps(
                        {
                            "pid": client._transport.delegate._process.pid,
                            "relay_port": client._aw_relay.server.sockets[0].getsockname()[1],
                        }
                    )
                )
                for denied in (True, False):
                    await client.query("Call the fixture tool once")
                    terminals = 0
                    async for message in client.receive_response():
                        if isinstance(message, ResultMessage):
                            assert not message.is_error
                            terminals += 1
                    assert terminals == 1
                    assert (directory / "effect").exists() == (
                        not denied and name != "native-permission-rule"
                    )
                    assert not client._aw_bridge.failure
                    assert not client._aw_bridge.invocations.active
                    assert not client._aw_bridge.invocations.pending
            assert len([r for r in fixture.REQUESTS if "messages" in r]) == 4
            if name == "permission-deny":
                assert permissions == [True, False]
            if name == "native-input-validation":
                assert [event for blocked, event in events if blocked] == []
            if name == "native-permission-rule":
                assert events == [(True, "PreToolUse"), (False, "PreToolUse")], events
            (directory / "native-events.json").write_text(json.dumps(events))
            results = [block for request in fixture.REQUESTS if "messages" in request
                       for message in request["messages"] for block in message.get("content", [])
                       if isinstance(block, dict) and block.get("type") == "tool_result"]
            assert results and results[0].get("is_error") is True
            cases.append({"case": name, "passed": True})
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=3)
            assert not thread.is_alive()
    (root / "result.json").write_text(json.dumps({"tasks": len(cases), "cases": cases, "passed": True}))
