"""A loopback TLS model endpoint with an attach-before-handshake barrier."""

import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import ssl
import threading


class ModelServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, certificate: Path, key: Path) -> None:
        super().__init__(("127.0.0.1", 0), ModelHandler)
        self.accepted = threading.Event()
        self.release = threading.Event()
        self.requests = []
        self.context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        self.context.load_cert_chain(certificate, key)
        self.context.set_alpn_protocols(["http/1.1"])
        self.thread = threading.Thread(target=self.serve_forever, daemon=True)
        self.thread.start()

    def get_request(self) -> tuple:
        connection, address = self.socket.accept()
        self.accepted.set()
        if not self.release.wait(15):
            connection.close()
            raise TimeoutError("probe readiness deadline")
        return self.context.wrap_socket(connection, server_side=True), address

    def close(self) -> None:
        self.release.set()
        self.shutdown()
        self.server_close()
        self.thread.join(5)


class RecordingStream:
    """Keep one synthetic HTTP request in memory for digest comparison only."""

    def __init__(self, source: object) -> None:
        self.source = source
        self.captured = bytearray()

    def read(self, size: int = -1) -> bytes:
        value = self.source.read(size)
        self.captured.extend(value)
        return value

    def readline(self, size: int = -1) -> bytes:
        value = self.source.readline(size)
        self.captured.extend(value)
        return value

    def close(self) -> None:
        self.source.close()


class ModelHandler(BaseHTTPRequestHandler):
    def setup(self) -> None:
        super().setup()
        self.rfile = RecordingStream(self.rfile)

    def log_message(self, *_args: object) -> None:
        pass

    def do_POST(self) -> None:
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        request = json.loads(body)
        self.server.requests.append(
            {
                "path": self.path,
                "length": len(body),
                "sha256": hashlib.sha256(body).hexdigest(),
                "wire_sha256": hashlib.sha256(self.rfile.captured).hexdigest(),
                "wire_length": len(self.rfile.captured),
                "marker_present": b"SYNTHETIC_TRANSPORT_PROBE" in body,
            }
        )
        if "count_tokens" in self.path:
            data, kind = b'{"input_tokens":1}', "application/json"
        elif "messages" in self.path:
            message = {
                "id": "fixture",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-6",
                "content": [],
                "stop_reason": None,
                "usage": {"input_tokens": 1, "output_tokens": 1},
            }
            events = [
                {"type": "message_start", "message": message},
                {
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {"type": "text", "text": ""},
                },
                {
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "text_delta", "text": "FIXTURE_OK"},
                },
                {"type": "content_block_stop", "index": 0},
                {
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn"},
                    "usage": {"output_tokens": 1},
                },
                {"type": "message_stop"},
            ]
            data = "".join(
                f'event: {e["type"]}\ndata: {json.dumps(e)}\n\n' for e in events
            ).encode()
            kind = "text/event-stream"
        else:
            completion = {
                "id": "fixture",
                "object": "chat.completion",
                "created": 1,
                "model": "fixture",
                "choices": [
                    {
                        "index": 0,
                        "message": {"role": "assistant", "content": "FIXTURE_OK"},
                        "finish_reason": "stop",
                    }
                ],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            }
            if request.get("stream"):
                completion["object"] = "chat.completion.chunk"
                completion["choices"][0]["delta"] = completion["choices"][0].pop("message")
                data = ("data: " + json.dumps(completion) + "\n\ndata: [DONE]\n\n").encode()
                kind = "text/event-stream"
            else:
                data, kind = json.dumps(completion).encode(), "application/json"
        self.send_response(200)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)
