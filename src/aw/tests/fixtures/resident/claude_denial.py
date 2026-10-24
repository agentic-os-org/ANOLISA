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
    for name in ("native-hook-deny", "permission-deny", "mcp-native-deny"):
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

        async def hook(payload, call_id, context):
            if denied:
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
            can_use_tool=permission if name == "permission-deny" else None,
            hooks={} if name == "permission-deny" else {"PreToolUse": [HookMatcher(hooks=[hook])]},
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
                    assert (directory / "effect").exists() == (not denied)
                    assert not client._aw_bridge.failure
                    assert not client._aw_bridge.invocations.active
                    assert not client._aw_bridge.invocations.pending
            assert len([r for r in fixture.REQUESTS if "messages" in r]) == 4
            if name == "permission-deny":
                assert permissions == [True, False]
            cases.append({"case": name, "passed": True})
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=3)
            assert not thread.is_alive()
    (root / "result.json").write_text(json.dumps({"tasks": 3, "cases": cases, "passed": True}))
