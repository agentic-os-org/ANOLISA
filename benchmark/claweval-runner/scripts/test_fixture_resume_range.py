#!/usr/bin/env python3

# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Regression test: fixture resume must not append a 200 body to a partial.

_FIXTURE_DOWNLOAD_SCRIPT opened the .part in "ab" before seeing the
response status. A Range-ignoring server answering 200 with the FULL
body therefore appended the full archive to the stale partial,
producing a corrupt archive that was still renamed into place and
reported as "downloaded" (sticky: later runs short-circuit on the
non-empty archive).

The script must pick the mode after the response: 206 -> append,
200-with-partial -> truncate and restart.
"""

import importlib.util
import io
import os
import subprocess
import sys
import tarfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

SCRIPTS_DIR = Path(__file__).resolve().parent

# Bytes that decode as a valid gzip tarball.
_TGZ = io.BytesIO()
with tarfile.open(fileobj=_TGZ, mode="w:gz") as t:
    data = b"hello-fixture"
    ti = tarfile.TarInfo("M001_clock/fixtures/config.json")
    ti.size = len(data)
    t.addfile(ti, io.BytesIO(data))
CONTENT = _TGZ.getvalue()
assert CONTENT[:2] == b"\x1f\x8b"  # sanity: served bytes are a gzip stream


def _load_setup_env():
    spec = importlib.util.spec_from_file_location(
        "setup_env_under_test", SCRIPTS_DIR / "setup_env.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class _Server200(BaseHTTPRequestHandler):
    """Always answers 200 with the full body (drops any Range header)."""

    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Length", str(len(CONTENT)))
        self.send_header("Content-Type", "application/octet-stream")
        self.end_headers()
        self.wfile.write(CONTENT)

    def log_message(self, *a):
        pass


class _Server206(BaseHTTPRequestHandler):
    """Honours Range: answers 206 with the requested suffix."""

    def do_GET(self):
        rng = self.headers.get("Range", "")
        if rng.startswith("bytes="):
            start = int(rng[len("bytes="):].split("-", 1)[0])
            body = CONTENT[start:]
            self.send_response(206)
            self.send_header("Content-Range",
                             f"bytes {start}-{len(CONTENT) - 1}/{len(CONTENT)}")
        else:
            body = CONTENT
            self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Content-Type", "application/octet-stream")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass


class FixtureResumeRangeTest(unittest.TestCase):
    def setUp(self):
        import tempfile
        self._tmp = tempfile.TemporaryDirectory()
        self.base = Path(self._tmp.name)
        self.archive = self.base / "fixtures.tar.gz"
        self.mod = _load_setup_env()

    def tearDown(self):
        self._tmp.cleanup()

    def _run_download(self, handler):
        server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        port = server.server_address[1]
        threading.Thread(target=server.serve_forever, daemon=True).start()
        try:
            script = self.mod._FIXTURE_DOWNLOAD_SCRIPT.format(
                url=f"http://127.0.0.1:{port}/fixtures.tar.gz",
                archive=str(self.archive),
                hf_dataset="claw-eval/Claw-Eval",
            )
            return subprocess.run(
                [sys.executable, "-c", script],
                capture_output=True, text=True, timeout=60,
            )
        finally:
            server.shutdown()
            server.server_close()

    def _assert_valid_archive(self):
        self.assertTrue(self.archive.exists())
        self.assertEqual(self.archive.read_bytes(), CONTENT)
        with tarfile.open(self.archive) as t:
            member = t.extractfile("M001_clock/fixtures/config.json")
            self.assertEqual(member.read(), b"hello-fixture")

    def test_range_ignoring_server_does_not_corrupt_archive(self):
        """200-with-full-body + stale partial -> valid archive, not append."""
        self.archive.with_suffix(self.archive.suffix + ".part").write_bytes(
            b"12345")  # partial from an interrupted run
        result = self._run_download(_Server200)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("restarting download from 0", result.stdout)
        self._assert_valid_archive()

    def test_range_honouring_server_still_appends(self):
        """206 resume path: partial + remainder -> valid archive."""
        part = self.archive.with_suffix(self.archive.suffix + ".part")
        part.write_bytes(CONTENT[:5])
        result = self._run_download(_Server206)
        self.assertEqual(result.returncode, 0, result.stderr)
        self._assert_valid_archive()

    def test_fresh_download_unchanged(self):
        """No partial, plain 200 -> valid archive (no behaviour change)."""
        result = self._run_download(_Server200)
        self.assertEqual(result.returncode, 0, result.stderr)
        self._assert_valid_archive()


if __name__ == "__main__":
    unittest.main()
