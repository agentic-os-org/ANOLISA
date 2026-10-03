#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for generate_image.py response payload validation.

Regression tests for the literal "b64:" fallback: when the OpenAI-compatible
response carried neither url nor b64_json, _compat returned "b64:" and _save
wrote a 0-byte .png while printing "Saved" (exit 0).
"""

import base64
import importlib.util
import io
import json
import os
import sys
import tempfile
import unittest
import urllib.request

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "generate_image.py")


def load_module():
    spec = importlib.util.spec_from_file_location("generate_image_test", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class FakeResp(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False


class TestPayloadValidation(unittest.TestCase):
    def setUp(self):
        self.module = load_module()

    def test_compat_no_payload_exits_1(self):
        """data[0] without url/b64_json must error with a response snippet."""
        payload = {"data": [{"revised_prompt": "a cat"}]}
        urllib.request.urlopen = lambda req, timeout=None: FakeResp(json.dumps(payload).encode())
        with self.assertRaises(SystemExit) as ctx:
            self.module._compat("a cat", "some-model", "1024*1024", "sk-x",
                                "https://example.invalid/v1")
        self.assertEqual(ctx.exception.code, 1)

    def test_compat_b64_payload_still_works(self):
        payload = {"data": [{"b64_json": base64.b64encode(b"img-bytes").decode()}]}
        urllib.request.urlopen = lambda req, timeout=None: FakeResp(json.dumps(payload).encode())
        src = self.module._compat("a cat", "some-model", "1024*1024", "sk-x",
                                  "https://example.invalid/v1")
        self.assertTrue(src.startswith("b64:"))
        self.assertNotEqual(src, "b64:")

    def test_save_empty_b64_marker_exits_1_without_file(self):
        """_save("b64:", tmp) must exit 1 and write nothing (was 0-byte file)."""
        with tempfile.TemporaryDirectory() as root:
            out = os.path.join(root, "nested", "img.png")
            with self.assertRaises(SystemExit) as ctx:
                self.module._save("b64:", out)
            self.assertEqual(ctx.exception.code, 1)
            self.assertFalse(os.path.exists(out))

    def test_save_valid_b64_writes_file(self):
        data = b"png" * 100
        with tempfile.TemporaryDirectory() as root:
            out = os.path.join(root, "img.png")
            self.module._save("b64:" + base64.b64encode(data).decode(), out)
            with open(out, "rb") as fh:
                self.assertEqual(fh.read(), data)


if __name__ == "__main__":
    unittest.main()
