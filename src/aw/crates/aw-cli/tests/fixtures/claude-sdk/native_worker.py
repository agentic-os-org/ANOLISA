"""Real SDK/bundled CLI adoption against a local deterministic model endpoint."""

from __future__ import annotations

import asyncio
from dataclasses import replace
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
import shlex
from pathlib import Path
import threading
from typing import Any

from aw_claude import create_client
from claude_agent_sdk import (
    ClaudeAgentOptions,
    HookMatcher,
    PermissionResultAllow,
    create_sdk_mcp_server,
    tool,
)

ROOT = Path.cwd()
MODE = os.environ.get("CLAUDE_TEST_MODE", "allow")
BASH = MODE.startswith("bash-")
PRETOOL = MODE in {"bash-pretool", "bash-pretool-deny"}
ORIGINAL = json.dumps({"numbers": list(range(50)), "ok": True}, indent=2)
REQUESTS: list[dict[str, Any]] = []
NATIVE_EVENTS: list[dict[str, Any]] = []


class Handler(BaseHTTPRequestHandler):
    """Serve only fixture data on an owned loopback port; no model or cloud calls."""

    def log_message(self, *args: Any) -> None:
        pass

    def do_POST(self) -> None:
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        REQUESTS.append(body)
        if self.path.endswith("count_tokens"):
            self.reply(json.dumps({"input_tokens": 1}).encode(), "application/json")
            return
        inference = sum("messages" in request for request in REQUESTS)
        done = inference % 2 == 0
        tool_input: dict[str, Any] = {"value": f"tool_native_{len(REQUESTS)}"}
        if BASH:
            tool_input = {
                "command": (
                    "exit 7"
                    if MODE in {"bash-failure", "bash-compress-tool-failure"}
                    else "printf %s " + shlex.quote(ORIGINAL) + "; touch effect"
                )
            }
        if PRETOOL:
            done = inference > 2
            tool_input = {"command": "git status"}
            if inference == 2:
                results = [
                    block
                    for message in body["messages"]
                    for block in message.get("content", [])
                    if isinstance(block, dict) and block.get("type") == "tool_result"
                ]
                text = (
                    json.dumps(results)
                    if not isinstance(results[-1]["content"], str)
                    else results[-1]["content"]
                )
                prefix = "[tokenless:rewrite] Retry this tool with arguments: "
                tool_input, _ = json.JSONDecoder().raw_decode(text.split(prefix, 1)[1])
                (ROOT / "retry.json").write_text(json.dumps(tool_input))
        blocks = (
            [{"type": "text", "text": "done"}]
            if done
            else [
                {
                    "type": "tool_use",
                    "id": f"tool_native_{len(REQUESTS)}",
                    "name": "Bash" if BASH else "mcp__local__touch",
                    "input": tool_input,
                }
            ]
        )
        if MODE == "parallel" and not done:
            blocks.append(
                {
                    **blocks[0],
                    "id": blocks[0]["id"] + "_second",
                    "input": {"value": blocks[0]["id"] + "_second"},
                }
            )
        message = {
            "id": "msg_local",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-6",
            "content": blocks,
            "stop_reason": "end_turn" if done else "tool_use",
            "stop_sequence": None,
            "usage": {"input_tokens": 1, "output_tokens": 1},
        }
        if not body.get("stream"):
            self.reply(json.dumps(message).encode(), "application/json")
            return
        events = [
            (
                "message_start",
                {
                    "type": "message_start",
                    "message": {**message, "content": [], "stop_reason": None},
                },
            )
        ]
        for index, block in enumerate(blocks):
            text = block["type"] == "text"
            initial = {"type": "text", "text": ""} if text else {**block, "input": {}}
            delta = (
                {"type": "text_delta", "text": block["text"]}
                if text
                else {"type": "input_json_delta", "partial_json": json.dumps(block["input"])}
            )
            events.extend(
                [
                    (
                        "content_block_start",
                        {"type": "content_block_start", "index": index, "content_block": initial},
                    ),
                    (
                        "content_block_delta",
                        {"type": "content_block_delta", "index": index, "delta": delta},
                    ),
                    ("content_block_stop", {"type": "content_block_stop", "index": index}),
                ]
            )
        events.extend(
            [
                (
                    "message_delta",
                    {
                        "type": "message_delta",
                        "delta": {"stop_reason": message["stop_reason"], "stop_sequence": None},
                        "usage": {"output_tokens": 1},
                    },
                ),
                ("message_stop", {"type": "message_stop"}),
            ]
        )
        data = "".join(f"event: {event}\ndata: {json.dumps(value)}\n\n" for event, value in events)
        self.reply(data.encode(), "text/event-stream")

    def reply(self, data: bytes, kind: str) -> None:
        self.send_response(200)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


@tool("touch", "Write a fixture marker", {"value": str})
async def touch(args: dict[str, Any]) -> dict[str, Any]:
    assert (ROOT / f"policy-{args['value']}").exists(), "tool executed before policy completion"
    (ROOT / "effect").write_text(args["value"])
    if MODE in {"failure", "mcp-error-compress"}:
        return {"content": [{"type": "text", "text": "fixture tool failure"}], "isError": True}
    return {"content": [{"type": "text", "text": "ok"}]}


async def pre(
    payload: dict[str, Any], call_id: str | None, context: dict[str, Any]
) -> dict[str, Any]:
    NATIVE_EVENTS.append(payload)
    if PRETOOL:
        return {"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "ask"}}
    if MODE == "native-deny":
        return {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": "original hook veto",
            }
        }
    if MODE == "rewrite":
        return {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "updatedInput": {"value": "rewritten"},
            }
        }
    if MODE == "native-allow-aw-deny" or MODE.startswith("native-hook-"):
        return {
            "hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow"}
        }
    return {}


async def post(
    payload: dict[str, Any], call_id: str | None, context: dict[str, Any]
) -> dict[str, Any]:
    NATIVE_EVENTS.append(payload)
    if MODE == "bash-compress-native-rewrite":
        return {
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "updatedToolOutput": {
                    **payload["tool_response"],
                    "stdout": "unchecked native replacement",
                },
            }
        }
    if PRETOOL and payload["hook_event_name"] == "PostToolUse":
        (ROOT / "effect").touch()
    return {}


async def permission(name: str, tool_input: dict[str, Any], context: Any) -> Any:
    (ROOT / "permission-called").touch()
    if PRETOOL:
        (ROOT / "permission-input.json").write_text(json.dumps(tool_input))
    return PermissionResultAllow(updated_input=None if PRETOOL else {"value": "rewritten"})


async def run(server: ThreadingHTTPServer) -> None:
    opts = ClaudeAgentOptions(
        model="claude-sonnet-4-6",
        cwd=str(ROOT),
        tools=["Bash"] if BASH else [],
        allowed_tools=(
            [] if MODE == "permission-rewrite" else ["Bash" if BASH else "mcp__local__touch"]
        ),
        mcp_servers={"local": create_sdk_mcp_server("local", tools=[touch])},
        setting_sources=[],
        can_use_tool=permission if MODE == "permission-rewrite" or PRETOOL else None,
        hooks={
            "PreToolUse": [HookMatcher(hooks=[pre])],
            "PostToolUse": [HookMatcher(hooks=[post])],
            "PostToolUseFailure": [HookMatcher(hooks=[post])],
        },
        env={
            "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_port}",
            "ANTHROPIC_API_KEY": "local-fixture",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
        },
        max_turns=3 if PRETOOL else 2,
    )
    if MODE == "permission-hook-rewrite":

        async def permission_hook(
            payload: dict[str, Any], call_id: str | None, context: dict[str, Any]
        ) -> dict[str, Any]:
            return {
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {"behavior": "allow", "updatedInput": {"value": "rewritten"}},
                }
            }

        opts = replace(
            opts, hooks={**opts.hooks, "PermissionRequest": [HookMatcher(hooks=[permission_hook])]}
        )
    if MODE == "permission-prompt":
        opts = replace(opts, permission_prompt_tool_name="mcp__local__permission")
    if MODE == "unready":
        return
    if MODE == "settings":
        opts = replace(opts, setting_sources=["project"])
    if MODE == "cli-path":
        opts = replace(opts, cli_path="/not/bundled")
    if MODE == "plugins":
        opts = replace(opts, plugins=[{"type": "local", "path": "/fixture-plugin"}])
    if MODE == "extra-args":
        opts = replace(opts, extra_args={"settings": "/fixture-settings"})
    if MODE == "strict-mcp-config":
        opts = replace(opts, extra_args={"strict-mcp-config": None})
    messages = []
    async with create_client(opts) as client:
        transport = client._transport
        (ROOT / "sdk-cli-process.json").write_text(
            json.dumps(
                {
                    "pid": transport._process.pid,
                    "command": transport._build_command(),
                    "cwd": str(ROOT),
                    "cli_path": transport._cli_path,
                    "lifetime": "client context",
                    "stop": f"kill -TERM {transport._process.pid}",
                }
            )
        )
        for turn in range(
            35
            if MODE in {"many-denials", "native-hook-invalid-chain"}
            else 2 if MODE == "multi-turn" else 1
        ):
            await client.query(f"Use the fixture tool once, turn {turn}")
            async for message in client.receive_response():
                messages.append(type(message).__name__)
    (ROOT / "report.json").write_text(
        json.dumps(
            {
                "events": NATIVE_EVENTS,
                "messages": messages,
                "requests": len(REQUESTS),
                "tool_results": [
                    block
                    for request in REQUESTS
                    if "messages" in request
                    for message in request["messages"]
                    for block in message.get("content", [])
                    if isinstance(block, dict) and block.get("type") == "tool_result"
                ],
                "original": ORIGINAL,
                "effect": (ROOT / "effect").exists(),
            }
        )
    )


def main() -> None:
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    (ROOT / "server.json").write_text(
        json.dumps(
            {
                "pid": os.getpid(),
                "port": server.server_port,
                "lifetime": "worker scope",
                "stop": f"kill -TERM {os.getpid()}",
            }
        )
    )
    try:
        asyncio.run(run(server))
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


if __name__ == "__main__":
    main()
