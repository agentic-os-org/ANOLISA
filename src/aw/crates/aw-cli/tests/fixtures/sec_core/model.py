"""Deterministic loopback model for stock QwenPaw App tool turns."""

import http.server
import json
import os
from pathlib import Path
import sys
import time

from runtime import save


def model_server(case):
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            tools = [item["function"]["name"] for item in body.get("tools", [])]
            # Tests reusing a Session assign each turn a distinct native call ID.
            identity = case / "tool-call-id.txt"
            call_id = identity.read_text() if identity.exists() else "call_aw_acceptance"
            seen = any(item["role"] == "tool" and item.get("tool_call_id") == call_id
                       for item in body["messages"])
            with (case / "model-requests.jsonl").open("a") as output:
                output.write(
                    json.dumps(
                        {
                            "tools": tools,
                            "tool_result_seen": seen,
                            "stream": body.get("stream"),
                            "tool_results": [m for m in body["messages"] if m["role"] == "tool"],
                        }
                    )
                    + "\n"
                )
            if not seen:
                request_path = case / "tool-call.json"
                tool = json.loads(request_path.read_text()) if request_path.exists() else {
                    "name": "execute_shell_command",
                    "arguments": {"command": (case / "tool-command.txt").read_text(), "timeout": 5},
                }
                if tool["name"] not in tools:
                    self.send_error(400, "requested native tool absent")
                    return
                delta = {
                    "role": "assistant",
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": call_id,
                            "type": "function",
                            "function": {
                                "name": tool["name"],
                                "arguments": json.dumps(tool["arguments"]),
                            },
                        }
                    ],
                }
                finish = "tool_calls"
            else:
                delta, finish = {
                    "role": "assistant",
                    "content": "AW acceptance complete.",
                }, "stop"
            base = {"id": "chatcmpl-aw-acceptance", "created": 1, "model": "aw-fixture"}
            usage = {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            if body.get("stream"):
                frames = [
                    {
                        **base,
                        "object": "chat.completion.chunk",
                        "choices": [{"index": 0, "delta": delta, "finish_reason": None}],
                    },
                    {
                        **base,
                        "object": "chat.completion.chunk",
                        "choices": [{"index": 0, "delta": {}, "finish_reason": finish}],
                        "usage": usage,
                    },
                ]
                payload = (
                    "".join("data: " + json.dumps(frame) + "\n\n" for frame in frames)
                    + "data: [DONE]\n\n"
                ).encode()
                content_type = "text/event-stream"
            else:
                for call in delta.get("tool_calls", []):
                    call.pop("index", None)
                payload = json.dumps(
                    {
                        **base,
                        "object": "chat.completion",
                        "usage": usage,
                        "choices": [{"index": 0, "message": delta, "finish_reason": finish}],
                    }
                ).encode()
                content_type = "application/json"
            self.send_response(200)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    with http.server.HTTPServer(("127.0.0.1", 0), Handler) as server:
        save(case / "model-server.json", {"pid": os.getpid(), "port": server.server_port})
        server.timeout = 0.5
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            server.handle_request()


if __name__ == "__main__":
    model_server(Path(sys.argv[1]))
