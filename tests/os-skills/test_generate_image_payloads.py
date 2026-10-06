#!/usr/bin/env python3
"""Regression tests for generate_image.py response-payload handling.

A response entry that carries neither ``url`` nor ``b64_json`` used to
become the literal string ``"b64:"`` — ``_save`` then decoded an empty
payload and reported a 0-byte output file as success. A wanx task that
SUCCEEDED without a usable result kept polling the finished task for
the full 120 x 2s loop and finally reported a misleading timeout.
"""

import importlib.util
import io
import json
import sys
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from unittest import mock

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src"
    / "os-skills"
    / "others"
    / "image-gen"
    / "scripts"
    / "generate_image.py"
)


def load_module():
    spec = importlib.util.spec_from_file_location("generate_image_under_test", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class FakeResponse:
    def __init__(self, payload):
        self._payload = payload

    def read(self):
        return json.dumps(self._payload).encode()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class CompatPayloadTest(unittest.TestCase):
    def setUp(self):
        self.module = load_module()

    def test_entry_without_image_fails_cleanly(self):
        def fake_urlopen(req, timeout=None):
            return FakeResponse({"data": [{"revised_prompt": "p"}]})

        with mock.patch.object(self.module.urllib.request, "urlopen", fake_urlopen):
            with redirect_stderr(io.StringIO()) as err:
                with self.assertRaises(SystemExit) as ctx:
                    self.module._compat("p", "m", "1024*1024", "k", "http://base")
        self.assertEqual(ctx.exception.code, 1)
        self.assertIn("No image", err.getvalue())

    def test_entry_with_url_returned(self):
        def fake_urlopen(req, timeout=None):
            return FakeResponse({"data": [{"url": "http://x/img.png"}]})

        with mock.patch.object(self.module.urllib.request, "urlopen", fake_urlopen):
            src = self.module._compat("p", "m", "1024*1024", "k", "http://base")
        self.assertEqual(src, "http://x/img.png")


class WanxSucceededWithoutImageTest(unittest.TestCase):
    def setUp(self):
        self.module = load_module()

    def test_succeeded_task_without_result_errors_fast(self):
        calls = {"n": 0}

        def fake_urlopen(req, timeout=None):
            calls["n"] += 1
            url = getattr(req, "full_url", "")
            if "image-synthesis" in str(url):
                return FakeResponse({"output": {"task_id": "t1"}})
            return FakeResponse({"output": {"task_status": "SUCCEEDED", "results": [{}]}})

        sleeps = []
        with mock.patch.object(self.module.urllib.request, "urlopen", fake_urlopen), \
                mock.patch.object(self.module.time, "sleep", sleeps.append), \
                redirect_stderr(io.StringIO()) as err:
            with self.assertRaises(SystemExit) as ctx:
                self.module._wanx("p", "m", "1024*1024", "k")

        self.assertEqual(ctx.exception.code, 1)
        # Must stop at the first SUCCEEDED poll, not run the 120-poll loop.
        self.assertLessEqual(
            calls["n"],
            3,
            f"a SUCCEEDED task without an image must error on first poll, "
            f"polled {calls['n']} times",
        )
        self.assertIn("SUCCEEDED", err.getvalue())


if __name__ == "__main__":
    unittest.main()
