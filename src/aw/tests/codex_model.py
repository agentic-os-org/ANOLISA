"""Deterministic Responses stream; only the pinned Codex runtime executes tools."""

import json
import re
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Thread


class Model:
    def __init__(self, directory, mode):
        self.directory = directory
        self.mode = mode
        self.requests = []
        self.calls = 0
        self.outputs = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                owner.requests.append(request)
                (directory / "requests.json").write_text(json.dumps(owner.requests, indent=2))
                items = owner.reply(request)
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                for item in items:
                    self.wfile.write(("data: " + json.dumps(item) + "\n\n").encode())
                self.wfile.flush()

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = Thread(target=self.server.serve_forever)
        self.thread.start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=3)
        assert not self.thread.is_alive()

    def reply(self, request):
        self.outputs = [item["output"] for item in request.get("input", [])
                        if isinstance(item, dict) and item.get("type") == "function_call_output"]
        command = None
        if self.calls == 0:
            command = "printf AW_NATIVE_OUTPUT; printf AW_NATIVE_ERROR >&2; exit 7"
            if self.mode in ("deny", "retry", "offline", "native-deny"):
                command = "printf ORIGINAL > AW_MUST_NOT_EXIST"
            if self.mode == "long":
                command = "sleep 1.2; printf AW_LONG_DONE"
        elif self.mode == "retry" and self.calls == 1:
            command = "printf CANDIDATE > AW_MUST_NOT_EXIST"
        if command is not None:
            self.calls += 1
            item = {"type": "function_call", "id": f"fc_{self.calls}", "call_id": f"call_{self.calls}",
                    "name": "exec_command", "arguments": json.dumps({"cmd": command, "yield_time_ms": 1000, "max_output_tokens": 1000})}
        elif self.mode == "long" and self.outputs and "Process running with session ID" in str(self.outputs[-1]):
            match = re.search(r"Process running with session ID (\d+)", str(self.outputs[-1]))
            assert match
            self.calls += 1
            assert self.calls < 5, "long-command polling exceeded fixture deadline"
            item = {"type": "function_call", "id": f"fc_{self.calls}", "call_id": f"call_{self.calls}",
                    "name": "write_stdin", "arguments": json.dumps({"session_id": int(match[1]), "chars": "", "yield_time_ms": 1000})}
        else:
            item = {"type": "message", "id": "message_final", "role": "assistant", "status": "completed",
                    "content": [{"type": "output_text", "text": "AW_CODEX_DONE", "annotations": []}]}
        response = {"id": f"response_{len(self.requests)}", "object": "response", "status": "completed",
                    "output": [item], "usage": {"input_tokens": 10, "output_tokens": 10, "total_tokens": 20}}
        events = [{"type": "response.created", "response": {**response, "status": "in_progress", "output": []}},
                  {"type": "response.output_item.added", "output_index": 0, "item": item}]
        if item["type"] == "message":
            events.append({"type": "response.output_text.delta", "output_index": 0, "content_index": 0,
                           "item_id": item["id"], "delta": "AW_CODEX_DONE"})
        events.extend([{"type": "response.output_item.done", "output_index": 0, "item": item},
                       {"type": "response.completed", "response": response}])
        return events
