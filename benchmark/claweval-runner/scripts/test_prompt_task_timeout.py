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

"""Regression test: _send_prompt must reach its timeout handler.

httpx.TimeoutException subclasses httpx.HTTPError, so catching HTTPError
first made the timeout-specific message unreachable dead code - every
timeout was logged with the generic "HTTP API call failed" wording.
"""

import contextlib
import importlib.util
import io
import json
import unittest
from pathlib import Path
from unittest import mock

import httpx

SCRIPT = Path(__file__).resolve().parent / "prompt_task.py"


def _load_prompt_task():
    spec = importlib.util.spec_from_file_location("prompt_task_under_test",
                                                  SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class PromptTaskTimeoutHandlerTest(unittest.TestCase):
    def setUp(self):
        self.mod = _load_prompt_task()
        import tempfile
        self._tmp = tempfile.TemporaryDirectory()
        cfg = Path(self._tmp.name) / "openclaw.json"
        cfg.write_text(json.dumps({
            "gateway": {"port": 18789, "auth": {"token": "tok"}},
        }), encoding="utf-8")
        self._cfg = str(cfg)

    def tearDown(self):
        self._tmp.cleanup()

    def _send(self, exc) -> str:
        buf = io.StringIO()
        with mock.patch.object(self.mod, "OPENCLAW_CONFIG", self._cfg), \
             mock.patch.object(httpx, "post", side_effect=exc), \
             contextlib.redirect_stdout(buf):
            result = self.mod._send_prompt("hi", "main", 60)
        self._log = buf.getvalue()
        return result

    def test_timeout_message_is_reachable(self):
        """TimeoutException hits the timeout branch (api_timeout=60+120)."""
        result = self._send(httpx.TimeoutException("read timed out"))
        self.assertEqual(result, "")
        self.assertIn("HTTP API call timed out after 180s", self._log)
        self.assertNotIn("HTTP API call failed", self._log)

    def test_generic_http_error_branch_still_works(self):
        """Non-timeout HTTPError keeps the generic message."""
        result = self._send(httpx.ConnectError("connection refused"))
        self.assertEqual(result, "")
        self.assertIn("HTTP API call failed", self._log)
        self.assertNotIn("timed out", self._log)

    def test_timeout_subclasses_http_error(self):
        """Pin the subclassing fact that makes handler ORDER load-bearing."""
        self.assertTrue(issubclass(httpx.TimeoutException, httpx.HTTPError))


if __name__ == "__main__":
    unittest.main()
