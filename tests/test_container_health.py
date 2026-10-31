"""Exercise the installed health command against real loopback HTTP sockets."""

import http.server
import json
import os
import socket
import subprocess
import sys
import threading
import time
import unittest
from pathlib import Path

PROBE = Path(__file__).resolve().parents[1] / "distribution" / "docker" / "healthcheck.py"


class HealthTests(unittest.TestCase):
    def run_probe(self, port: int | str, required: str = "1") -> tuple[int, dict]:
        process = subprocess.run(
            [sys.executable, str(PROBE)],
            env={
                **os.environ,
                "ANOLISA_AGENTSIGHT_PORT": str(port),
                "ANOLISA_REQUIRE_AGENTSIGHT": required,
            },
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        return process.returncode, json.loads(process.stdout)

    def test_http_contract(self) -> None:
        for status, payload, ready in [
            (200, b'{"status":"ok","version":"test"}', True),
            (503, b'{"status":"ok"}', False),
            (302, b"", False),
            (200, b"not-json", False),
            (200, b"[]", False),
            (200, b'{"status":"starting"}', False),
            (200, b"x" * 65537, False),
        ]:
            with self.subTest(status=status, payload=payload[:40]):

                class Handler(http.server.BaseHTTPRequestHandler):
                    def do_GET(self) -> None:
                        self.send_response(status)
                        self.end_headers()
                        self.wfile.write(payload)

                    def log_message(self, *args: object) -> None:
                        pass

                server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
                thread = threading.Thread(target=server.handle_request)
                thread.start()
                try:
                    code, record = self.run_probe(server.server_port)
                    self.assertEqual(code, int(not ready))
                    self.assertEqual(record["status"], "ready" if ready else "degraded")
                finally:
                    thread.join(timeout=5)
                    server.server_close()

    def test_unavailable_optional_and_required(self) -> None:
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
            for required in ("0", "1"):
                code, record = self.run_probe(port, required)
                self.assertEqual(code, int(required))
                self.assertEqual(record["status"], "degraded")

    def test_stalled_server_times_out(self) -> None:
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen()
            started = time.monotonic()
            code, record = self.run_probe(listener.getsockname()[1])
            self.assertEqual(code, 1)
            self.assertEqual(record["status"], "degraded")
            self.assertLess(time.monotonic() - started, 4)

    def test_deep_json_obeys_optional_policy(self) -> None:
        payload = b'{"status":"ok","extra":' + b"[" * 10000 + b"0" + b"]" * 10000 + b"}"
        for required in ("0", "1"):
            with self.subTest(required=required):

                class Handler(http.server.BaseHTTPRequestHandler):
                    def do_GET(self) -> None:
                        self.send_response(200)
                        self.end_headers()
                        self.wfile.write(payload)

                    def log_message(self, *args: object) -> None:
                        pass

                server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
                thread = threading.Thread(target=server.handle_request)
                thread.start()
                try:
                    code, record = self.run_probe(server.server_port, required)
                    self.assertEqual(code, int(required))
                    self.assertEqual(record["status"], "degraded")
                finally:
                    thread.join(timeout=5)
                    server.server_close()

    def test_invalid_configuration(self) -> None:
        for port, required in [(0, "1"), (65536, "1"), (8080, "true"), ("9" * 5000, "1")]:
            code, record = self.run_probe(port, required)
            self.assertEqual(code, 1)
            self.assertEqual(record["status"], "blocked")


if __name__ == "__main__":
    unittest.main()
