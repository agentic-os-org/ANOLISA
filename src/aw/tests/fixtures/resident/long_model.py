"""Bounded deterministic model for repeated IDs and long mixed Bash/MCP executions."""

import json
import shlex
from http.server import BaseHTTPRequestHandler

ORIGINAL = json.dumps(
    {"rows": [{"id": n, "description": "stable fixture data"} for n in range(80)]}, indent=2
)


class Handler(BaseHTTPRequestHandler):
    state = None

    def log_message(self, *args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.path.endswith("count_tokens"):
            self.reply(b'{"input_tokens":1}', "application/json")
            return
        state = self.state
        state["requests"] += 1
        results = [
            block
            for message in body["messages"]
            if isinstance(message.get("content"), list)
            for block in message["content"]
            if block.get("type") == "tool_result"
        ]
        if results:
            latest = results[-1]["content"]
            if isinstance(latest, list):
                latest = "\n".join(block.get("text", "") for block in latest)
            assert not results[-1].get("is_error", False), results[-1]
            assert ORIGINAL not in latest and len(latest) < len(
                ORIGINAL
            ), "uncompressed result reached the next model request"
            assert results[-1]["tool_use_id"] != "reused-native-id"
            state["adopted"] += 1
        if state["remaining"]:
            state["remaining"] -= 1
            state["issued"] += 1
            mcp = state["issued"] % 2 == 1
            blocks = [
                {
                    "type": "tool_use",
                    "id": "reused-native-id",
                    "name": "mcp__local__touch" if mcp else "Bash",
                    "input": (
                        {"value": "same-input"}
                        if mcp
                        else {"command": "printf %s " + shlex.quote(ORIGINAL) + "; touch effect"}
                    ),
                }
            ]
        else:
            blocks = [{"type": "text", "text": "done"}]
        message = {
            "id": "model-message",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-6",
            "content": blocks,
            "stop_reason": "tool_use" if blocks[0]["type"] == "tool_use" else "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1},
        }
        events = [
            {"type": "message_start", "message": {**message, "content": [], "stop_reason": None}}
        ]
        for index, block in enumerate(blocks):
            text = block["type"] == "text"
            events += [
                {
                    "type": "content_block_start",
                    "index": index,
                    "content_block": (
                        {"type": "text", "text": ""} if text else {**block, "input": {}}
                    ),
                },
                {
                    "type": "content_block_delta",
                    "index": index,
                    "delta": (
                        {"type": "text_delta", "text": block["text"]}
                        if text
                        else {
                            "type": "input_json_delta",
                            "partial_json": json.dumps(block["input"]),
                        }
                    ),
                },
                {"type": "content_block_stop", "index": index},
            ]
        events += [
            {
                "type": "message_delta",
                "delta": {"stop_reason": message["stop_reason"]},
                "usage": {"output_tokens": 1},
            },
            {"type": "message_stop"},
        ]
        self.reply(
            "".join(
                f'event: {event["type"]}\ndata: {json.dumps(event)}\n\n' for event in events
            ).encode(),
            "text/event-stream",
        )

    def reply(self, body, content_type):
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
