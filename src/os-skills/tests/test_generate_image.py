#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""generate_image.py must not save an empty image as a success.

A response whose `data[0]` carries neither `url` nor `b64_json` (some
OpenAI-compatible servers return only `revised_prompt`) used to produce the
literal source `"b64:"`, which wrote a 0-byte png and exited 0.
"""

import base64
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPTS_DIR = Path(__file__).resolve().parents[1] / "others" / "image-gen" / "scripts"
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

import generate_image

sys.dont_write_bytecode = True


def http_response(payload):
    """Stand-in for urlopen's context manager."""
    handle = mock.MagicMock()
    handle.read.return_value = json.dumps(payload).encode()
    context = mock.MagicMock()
    context.__enter__.return_value = handle
    context.__exit__.return_value = False
    return context


class TestSaveRejectsEmptyImage(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmpdir.cleanup)
        self.path = Path(self.tmpdir.name) / "img.png"

    def test_empty_b64_payload_errors_and_writes_nothing(self):
        with self.assertRaises(SystemExit) as raised:
            generate_image._save("b64:", str(self.path))

        self.assertEqual(raised.exception.code, 1)
        self.assertFalse(self.path.exists())

    def test_invalid_base64_errors_and_writes_nothing(self):
        with self.assertRaises(SystemExit) as raised:
            generate_image._save("b64:not base64!", str(self.path))

        self.assertEqual(raised.exception.code, 1)
        self.assertFalse(self.path.exists())

    def test_empty_download_errors_and_writes_nothing(self):
        context = mock.MagicMock()
        context.__enter__.return_value.read.return_value = b""
        context.__exit__.return_value = False
        with (
            mock.patch.object(generate_image.urllib.request, "urlopen", return_value=context),
            self.assertRaises(SystemExit) as raised,
        ):
            generate_image._save("https://example.invalid/img.png", str(self.path))

        self.assertEqual(raised.exception.code, 1)
        self.assertFalse(self.path.exists())

    def test_real_b64_payload_is_written(self):
        payload = base64.b64encode(b"PNGDATA").decode()

        generate_image._save("b64:" + payload, str(self.path))

        self.assertEqual(self.path.read_bytes(), b"PNGDATA")


class TestCompatRequiresPayload(unittest.TestCase):
    def _compat(self, payload):
        with mock.patch.object(
            generate_image.urllib.request, "urlopen", return_value=http_response(payload)
        ):
            return generate_image._compat("p", "model", "1024*1024", "key", "https://api.invalid/v1")

    def test_response_without_url_or_b64_errors(self):
        with (
            mock.patch("sys.stderr", new=io.StringIO()) as err,
            self.assertRaises(SystemExit) as raised,
        ):
            self._compat({"data": [{"revised_prompt": "a cat"}]})

        self.assertEqual(raised.exception.code, 1)
        self.assertIn("no image payload", err.getvalue())

    def test_url_payload_is_returned(self):
        self.assertEqual(self._compat({"data": [{"url": "https://cdn.invalid/a.png"}]}),
                         "https://cdn.invalid/a.png")

    def test_b64_payload_is_returned(self):
        self.assertEqual(self._compat({"data": [{"b64_json": "QUJD"}]}), "b64:QUJD")


if __name__ == "__main__":
    unittest.main()
