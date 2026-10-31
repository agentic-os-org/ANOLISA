#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for image-gen empty-response handling.

Regression tests for the silent zero-byte output: when the generation API
returns a response item that carries neither a url nor a base64 payload,
_compat built the string "b64:" and _save decoded the empty payload,
wrote a 0-byte file, printed "Saved ... (0.0KB)" and exited 0 - so a
rejected/empty generation was reported as a successful delivery.
"""

import io
import os
import sys
import tempfile
import unittest
from unittest.mock import MagicMock, patch

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import generate_image as gi  # noqa: E402


def _http_response(payload):
    response = MagicMock()
    response.read = MagicMock(return_value=json_bytes(payload))
    response.__enter__ = MagicMock(return_value=response)
    response.__exit__ = MagicMock(return_value=False)
    return response


def json_bytes(payload):
    import json
    return json.dumps(payload).encode()


class TestEmptyImageResponse(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)

    def test_compat_rejects_payload_without_image(self):
        """响应项既无 url 也无 b64_json 时必须报错退出。

        修复前：返回 "b64:"+"" 即 "b64:"，下游解码为空、写出 0 字节
        文件并打印 Saved (0.0KB)、exit 0——空结果被当作成功交付。
        """
        with patch.object(gi.urllib.request, "urlopen",
                          return_value=_http_response({"data": [{}]})):
            with self.assertRaises(SystemExit) as ctx:
                gi._compat("a cat", "wanx2.1-t2i-turbo", "1024*1024", "k", "https://x")
        self.assertNotEqual(ctx.exception.code, 0)

    def test_save_rejects_empty_b64_payload(self):
        """_save 对空 base64 载荷必须报错，不得写出 0 字节文件。"""
        path = os.path.join(self._tmp.name, "img.png")
        with self.assertRaises(SystemExit) as ctx:
            gi._save("b64:", path)
        self.assertNotEqual(ctx.exception.code, 0)
        self.assertFalse(os.path.exists(path), "不得留下 0 字节交付物")

    def test_save_writes_valid_b64_payload(self):
        """正常链路保护：有效载荷照常落盘。"""
        import base64
        path = os.path.join(self._tmp.name, "img.png")
        gi._save("b64:" + base64.b64encode(b"hello").decode(), path)
        with open(path, "rb") as fh:
            self.assertEqual(fh.read(), b"hello")


if __name__ == "__main__":
    unittest.main()
