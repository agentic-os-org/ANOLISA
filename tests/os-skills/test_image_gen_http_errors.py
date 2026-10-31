#!/usr/bin/env python3
"""Image endpoint errors must not silently switch generation providers.

Original regression baseline on unchanged main 5351c892f: seven test methods,
14 failing error subcases (SystemExit not raised); successful/fallback controls
pass. Review regressions additionally cover visible fallback diagnostics,
empty responses, bounded reads, control characters and credential redaction.
"""

import http.client
import importlib.util
import io
import json
import socket
import struct
import sys
import threading
import unittest
import urllib.error
from contextlib import ExitStack, contextmanager, redirect_stderr
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

SCRIPT = (
    Path(__file__).resolve().parents[2] / "src/os-skills/others/image-gen/scripts/generate_image.py"
)
SPEC = importlib.util.spec_from_file_location("image_gen_http_errors", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
IMAGE_GEN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(IMAGE_GEN)

BASE = "https://example.invalid/v1"
PROMPT = "a fixture landscape"
MODEL = "compatible-image-model"
SIZE = "1024*1024"
KEY = "fixture-key"


@contextmanager
def endpoint(status, payload):
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            requests.append(
                {
                    "path": self.path,
                    "authorization": self.headers.get("Authorization"),
                    "body": json.loads(self.rfile.read(int(self.headers["Content-Length"]))),
                }
            )
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, format, *args):
            pass

    with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
        worker = threading.Thread(
            target=server.serve_forever, kwargs={"poll_interval": 0.01}, daemon=True
        )
        worker.start()
        try:
            yield f"http://127.0.0.1:{server.server_port}/v1", requests
        finally:
            server.shutdown()
            worker.join(timeout=5)
            if worker.is_alive():
                raise RuntimeError("fixture server did not stop")


@contextmanager
def resetting_endpoint(status):
    read_started = threading.Event()

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            self.rfile.read(int(self.headers["Content-Length"]))
            self.send_response(status)
            self.send_header("Content-Length", "4096")
            self.end_headers()
            self.wfile.write(b"partial")
            self.wfile.flush()
            # The client has already parsed the HTTP error before the reset.
            # This avoids turning the fixture into a header/transport failure.
            if not read_started.wait(5):
                raise RuntimeError("client never started reading the HTTP error")
            layout = "hh" if sys.platform == "win32" else "ii"
            self.connection.setsockopt(
                socket.SOL_SOCKET, socket.SO_LINGER, struct.pack(layout, 1, 0)
            )
            self.close_connection = True
            self.connection.close()

        def log_message(self, format, *args):
            pass

    with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
        worker = threading.Thread(
            target=server.serve_forever, kwargs={"poll_interval": 0.01}, daemon=True
        )
        worker.start()
        try:
            yield f"http://127.0.0.1:{server.server_port}/v1", read_started
        finally:
            read_started.set()
            server.shutdown()
            worker.join(timeout=5)
            if worker.is_alive():
                raise RuntimeError("reset fixture server did not stop")


class ImageHttpErrorTests(unittest.TestCase):
    def call_compat(self, base=BASE):
        return IMAGE_GEN._compat(PROMPT, MODEL, SIZE, KEY, base)

    def test_http_errors_preserve_status_and_body_without_fallback(self):
        for status in (400, 401, 403, 408, 409, 422, 429, 500, 502, 503, 504):
            with self.subTest(status=status):
                body = b'{"error":{"message":"fixture endpoint rejected request"}}'
                error = urllib.error.HTTPError(
                    f"{BASE}/images/generations", status, "fixture error", None, io.BytesIO(body)
                )
                stderr = io.StringIO()
                with ExitStack() as stack:
                    stack.enter_context(
                        patch.object(IMAGE_GEN.urllib.request, "urlopen", side_effect=error)
                    )
                    wanx = stack.enter_context(
                        patch.object(IMAGE_GEN, "_wanx", return_value="unexpected fallback")
                    )
                    stack.enter_context(redirect_stderr(stderr))
                    exited = stack.enter_context(self.assertRaises(SystemExit))
                    self.call_compat()
                self.assertEqual(exited.exception.code, 1)
                self.assertIn(f"HTTP {status}", stderr.getvalue())
                self.assertIn(body.decode(), stderr.getvalue())
                wanx.assert_not_called()

    def test_non_utf8_error_body_keeps_the_http_diagnostic(self):
        error = urllib.error.HTTPError(
            f"{BASE}/images/generations", 401, "fixture error", None, io.BytesIO(b"denied: \xff")
        )
        stderr = io.StringIO()
        with ExitStack() as stack:
            stack.enter_context(
                patch.object(IMAGE_GEN.urllib.request, "urlopen", side_effect=error)
            )
            wanx = stack.enter_context(
                patch.object(IMAGE_GEN, "_wanx", return_value="unexpected fallback")
            )
            stack.enter_context(redirect_stderr(stderr))
            exited = stack.enter_context(self.assertRaises(SystemExit))
            self.call_compat()
        self.assertEqual(exited.exception.code, 1)
        self.assertIn("HTTP 401 denied: \ufffd", stderr.getvalue())
        wanx.assert_not_called()

    def test_unsupported_route_still_uses_wanx(self):
        for status in sorted(IMAGE_GEN._FALLBACK_STATUS):
            with self.subTest(status=status):
                error = urllib.error.HTTPError(
                    f"{BASE}/images/generations", status, "unsupported", None, io.BytesIO(b"")
                )
                stderr = io.StringIO()
                with ExitStack() as stack:
                    stack.enter_context(redirect_stderr(stderr))
                    stack.enter_context(
                        patch.object(IMAGE_GEN.urllib.request, "urlopen", side_effect=error)
                    )
                    wanx = stack.enter_context(
                        patch.object(IMAGE_GEN, "_wanx", return_value="fallback image")
                    )
                    self.assertEqual(self.call_compat(), "fallback image")
                wanx.assert_called_once_with(PROMPT, MODEL, SIZE, KEY)
                self.assertIn(f"WARN: HTTP {status}", stderr.getvalue())
                self.assertIn("unsupported", stderr.getvalue())
                self.assertIn("falling back to Wanx", stderr.getvalue())

    def assert_error_diagnostic(self, body, reason="fixture error", key=KEY):
        error = urllib.error.HTTPError(
            f"{BASE}/images/generations", 401, reason, None, io.BytesIO(body)
        )
        stderr = io.StringIO()
        with ExitStack() as stack:
            stack.enter_context(
                patch.object(IMAGE_GEN.urllib.request, "urlopen", side_effect=error)
            )
            wanx = stack.enter_context(patch.object(IMAGE_GEN, "_wanx"))
            stack.enter_context(redirect_stderr(stderr))
            exited = stack.enter_context(self.assertRaises(SystemExit))
            IMAGE_GEN._compat(PROMPT, MODEL, SIZE, key, BASE)
        self.assertEqual(exited.exception.code, 1)
        wanx.assert_not_called()
        return stderr.getvalue()

    def test_empty_body_uses_the_http_reason(self):
        for body in (b"", b"  \n\t"):
            with self.subTest(body=body):
                self.assertIn(
                    "HTTP 401 Unauthorized", self.assert_error_diagnostic(body, "Unauthorized")
                )

    def test_large_error_response_is_read_and_logged_with_a_bound(self):
        body = io.BytesIO(b"x" * 5_000_000)
        error = urllib.error.HTTPError(
            f"{BASE}/images/generations", 401, "fixture error", None, body
        )
        stderr = io.StringIO()
        with ExitStack() as stack:
            stack.enter_context(
                patch.object(IMAGE_GEN.urllib.request, "urlopen", side_effect=error)
            )
            wanx = stack.enter_context(patch.object(IMAGE_GEN, "_wanx"))
            read = stack.enter_context(patch.object(error, "read", wraps=error.read))
            stack.enter_context(redirect_stderr(stderr))
            stack.enter_context(self.assertRaises(SystemExit))
            self.call_compat()
        read.assert_called_once_with(IMAGE_GEN._ERROR_DETAIL_LIMIT + 1)
        self.assertEqual(body.tell(), IMAGE_GEN._ERROR_DETAIL_LIMIT + 1)
        self.assertLess(len(stderr.getvalue()), 4200)
        self.assertIn("[truncated]", stderr.getvalue())
        wanx.assert_not_called()

        self.assertNotIn(
            "[truncated]", self.assert_error_diagnostic(b"x" * IMAGE_GEN._ERROR_DETAIL_LIMIT)
        )
        self.assertIn(
            "[truncated]", self.assert_error_diagnostic(b"x" * (IMAGE_GEN._ERROR_DETAIL_LIMIT + 1))
        )

    def test_body_read_failures_preserve_fallback_and_http_errors(self):
        failures = (
            ConnectionResetError("fixture reset"),
            http.client.HTTPException("fixture HTTP failure"),
            ValueError("closed stream"),
            KeyError("file"),
        )
        for status in (404, 405, 501, 503):
            for failure in failures:
                with self.subTest(status=status, failure=type(failure).__name__):
                    error = urllib.error.HTTPError(
                        f"{BASE}/images/generations", status, "fixture reason", None, io.BytesIO()
                    )
                    stderr = io.StringIO()
                    with ExitStack() as stack:
                        stack.enter_context(
                            patch.object(IMAGE_GEN.urllib.request, "urlopen", side_effect=error)
                        )
                        stack.enter_context(patch.object(error, "read", side_effect=failure))
                        wanx = stack.enter_context(
                            patch.object(IMAGE_GEN, "_wanx", return_value="fallback image")
                        )
                        stack.enter_context(redirect_stderr(stderr))
                        if status in IMAGE_GEN._FALLBACK_STATUS:
                            self.assertEqual(self.call_compat(), "fallback image")
                            wanx.assert_called_once_with(PROMPT, MODEL, SIZE, KEY)
                        else:
                            with self.assertRaises(SystemExit) as exited:
                                self.call_compat()
                            self.assertEqual(exited.exception.code, 1)
                            wanx.assert_not_called()
                    self.assertIn(f"HTTP {status}", stderr.getvalue())
                    self.assertIn("fixture reason", stderr.getvalue())
                    self.assertNotIn("Traceback", stderr.getvalue())

    def test_missing_error_stream_preserves_fallback_and_http_errors(self):
        for status in (404, 503):
            with self.subTest(status=status):
                error = urllib.error.HTTPError(
                    f"{BASE}/images/generations", status, "no stream", None, None
                )
                stderr = io.StringIO()
                with ExitStack() as stack:
                    stack.enter_context(
                        patch.object(IMAGE_GEN.urllib.request, "urlopen", side_effect=error)
                    )
                    wanx = stack.enter_context(
                        patch.object(IMAGE_GEN, "_wanx", return_value="fallback image")
                    )
                    stack.enter_context(redirect_stderr(stderr))
                    if status == 404:
                        self.assertEqual(self.call_compat(), "fallback image")
                        wanx.assert_called_once_with(PROMPT, MODEL, SIZE, KEY)
                    else:
                        with self.assertRaises(SystemExit) as exited:
                            self.call_compat()
                        self.assertEqual(exited.exception.code, 1)
                        wanx.assert_not_called()
                self.assertIn(f"HTTP {status}", stderr.getvalue())
                self.assertIn("no stream", stderr.getvalue())

    def test_real_body_reset_preserves_fallback_and_http_errors(self):
        for status in (404, 405, 501, 503):
            with self.subTest(status=status):
                stderr = io.StringIO()
                detail = IMAGE_GEN._http_error_detail
                with ExitStack() as stack:
                    base, read_started = stack.enter_context(resetting_endpoint(status))

                    def read_detail(error, key):
                        read_started.set()
                        return detail(error, key)

                    stack.enter_context(
                        patch.object(IMAGE_GEN, "_http_error_detail", side_effect=read_detail)
                    )
                    wanx = stack.enter_context(
                        patch.object(IMAGE_GEN, "_wanx", return_value="fallback image")
                    )
                    stack.enter_context(redirect_stderr(stderr))
                    if status in IMAGE_GEN._FALLBACK_STATUS:
                        self.assertEqual(self.call_compat(base), "fallback image")
                        wanx.assert_called_once_with(PROMPT, MODEL, SIZE, KEY)
                    else:
                        with self.assertRaises(SystemExit) as exited:
                            self.call_compat(base)
                        self.assertEqual(exited.exception.code, 1)
                        wanx.assert_not_called()
                self.assertIn(f"HTTP {status}", stderr.getvalue())
                self.assertNotIn("Traceback", stderr.getvalue())

    def test_short_placeholder_keys_do_not_corrupt_diagnostics(self):
        body = b'{"error":"gpt-image-1 does not exist; dummy EMPTY", "request_id":"req_1x2"}'
        for key in ("1", "x", "dummy", "EMPTY"):
            with self.subTest(key=key):
                self.assertIn(body.decode(), self.assert_error_diagnostic(body, key=key))

    def test_remote_whitespace_cannot_forge_output_lines(self):
        output = self.assert_error_diagnostic(
            b"bad request\nSaved out.png (12.3KB)\nERROR: HTTP 200\tOK"
        )
        self.assertEqual(output.count("\n"), 1)
        self.assertNotIn("\t", output)
        self.assertIn("bad request Saved out.png (12.3KB) ERROR: HTTP 200 OK", output)

    def test_reason_bytes_and_blank_truncated_body_remain_useful(self):
        self.assertIn("HTTP 401 Unauthorized", self.assert_error_diagnostic(b"", b"Unauthorized"))
        output = self.assert_error_diagnostic(b" " * (IMAGE_GEN._ERROR_DETAIL_LIMIT + 1), "reason")
        self.assertIn("HTTP 401 reason [truncated]", output)

    def test_remote_control_characters_are_removed(self):
        output = self.assert_error_diagnostic(b"denied: \x1b[2J\x1b[H\x00\r\x7f")
        self.assertIn("denied:", output)
        for control in ("\x1b", "\x00", "\r", "\x7f"):
            self.assertNotIn(control, output)

    def test_credentials_are_redacted_in_body_and_empty_body_reason(self):
        secrets = (KEY, "different-bearer.token_123", "sk-fixture_secret_123")
        message = f"api_key={KEY}; Authorization: Bearer {secrets[1]}; token={secrets[2]}"
        for body, reason in ((message.encode(), "unused"), (b"", message)):
            with self.subTest(body=bool(body)):
                output = self.assert_error_diagnostic(body, reason)
                for secret in secrets:
                    self.assertNotIn(secret, output)
                self.assertEqual(output.count("<redacted>"), 3)

    def test_fallback_retains_safe_first_hop_diagnostics_when_wanx_fails(self):
        body = f"model_not_found; Authorization: Bearer {KEY}".encode()
        error = urllib.error.HTTPError(
            f"{BASE}/images/generations", 404, "Not Found", None, io.BytesIO(body)
        )
        stderr = io.StringIO()
        with ExitStack() as stack:
            stack.enter_context(
                patch.object(IMAGE_GEN.urllib.request, "urlopen", side_effect=error)
            )
            wanx = stack.enter_context(patch.object(IMAGE_GEN, "_wanx", side_effect=SystemExit(1)))
            stack.enter_context(redirect_stderr(stderr))
            stack.enter_context(self.assertRaises(SystemExit))
            self.call_compat()
        wanx.assert_called_once_with(PROMPT, MODEL, SIZE, KEY)
        self.assertIn("HTTP 404", stderr.getvalue())
        self.assertIn("model_not_found", stderr.getvalue())
        self.assertIn("falling back to Wanx", stderr.getvalue())
        self.assertNotIn(KEY, stderr.getvalue())

    def test_successful_url_and_base64_responses_are_unchanged(self):
        for item, expected in (
            ({"url": "https://example.invalid/image.png"}, "https://example.invalid/image.png"),
            ({"b64_json": "aW1hZ2U="}, "b64:aW1hZ2U="),
        ):
            with self.subTest(item=item):
                response = io.BytesIO(json.dumps({"data": [item]}).encode())
                with ExitStack() as stack:
                    stack.enter_context(
                        patch.object(IMAGE_GEN.urllib.request, "urlopen", return_value=response)
                    )
                    wanx = stack.enter_context(patch.object(IMAGE_GEN, "_wanx"))
                    self.assertEqual(self.call_compat(), expected)
                wanx.assert_not_called()

    def test_transport_errors_are_not_retried_against_wanx(self):
        error = urllib.error.URLError("fixture connection refused")
        with ExitStack() as stack:
            stack.enter_context(
                patch.object(IMAGE_GEN.urllib.request, "urlopen", side_effect=error)
            )
            wanx = stack.enter_context(patch.object(IMAGE_GEN, "_wanx"))
            raised = stack.enter_context(self.assertRaises(urllib.error.URLError))
            self.call_compat()
        self.assertIs(raised.exception, error)
        wanx.assert_not_called()

    def test_real_http_errors_keep_the_original_endpoint_response(self):
        for status in (401, 503):
            with self.subTest(status=status):
                body = b'{"error":{"message":"loopback endpoint failure"}}'
                stderr = io.StringIO()
                with ExitStack() as stack:
                    (base, requests) = stack.enter_context(endpoint(status, body))
                    wanx = stack.enter_context(
                        patch.object(IMAGE_GEN, "_wanx", return_value="unexpected fallback")
                    )
                    stack.enter_context(redirect_stderr(stderr))
                    exited = stack.enter_context(self.assertRaises(SystemExit))
                    self.call_compat(base)
                self.assertEqual(exited.exception.code, 1)
                self.assertIn(f"HTTP {status}", stderr.getvalue())
                self.assertIn(body.decode(), stderr.getvalue())
                self.assertEqual(len(requests), 1)
                wanx.assert_not_called()

    def test_real_http_success_preserves_the_request_and_image(self):
        body = json.dumps({"data": [{"url": "https://example.invalid/image.png"}]}).encode()
        with ExitStack() as stack:
            (base, requests) = stack.enter_context(endpoint(200, body))
            wanx = stack.enter_context(patch.object(IMAGE_GEN, "_wanx"))
            self.assertEqual(self.call_compat(base), "https://example.invalid/image.png")
        self.assertEqual(
            requests,
            [
                {
                    "path": "/v1/images/generations",
                    "authorization": f"Bearer {KEY}",
                    "body": {
                        "model": MODEL,
                        "prompt": PROMPT,
                        "size": SIZE,
                        "n": 1,
                        "response_format": "url",
                    },
                }
            ],
        )
        wanx.assert_not_called()


if __name__ == "__main__":
    unittest.main()
