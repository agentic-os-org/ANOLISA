#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for generate_image.py _wanx terminal-state handling.

Regression tests for a SUCCEEDED wanx task whose results entry carries
neither url nor b64_image: the loop used to fall through and re-poll 120
times (240 s + 120 API calls) before reporting a bogus "Timeout".
"""

import importlib.util
import io
import json
import os
import sys
import time
import unittest
import urllib.request

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "generate_image.py")


def load_module():
    spec = importlib.util.spec_from_file_location("generate_image_wanx_test", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class FakeResp(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False


class WanxHarness:
    """Stub urlopen (submit + poll) with a patched time.sleep, per audit."""

    def __init__(self, module, poll_payload):
        self.module = module
        self.poll_payload = poll_payload
        self.polls = 0
        self.real_urlopen = urllib.request.urlopen
        self.real_sleep = time.sleep

    def __enter__(self):
        harness = self

        def fake_urlopen(req, timeout=None):
            if "image-synthesis" in req.full_url:
                return FakeResp(json.dumps({"output": {"task_id": "T1"}}).encode())
            harness.polls += 1
            return FakeResp(json.dumps({"output": harness.poll_payload}).encode())

        urllib.request.urlopen = fake_urlopen
        time.sleep = lambda s: None
        return self

    def __exit__(self, *a):
        urllib.request.urlopen = self.real_urlopen
        time.sleep = self.real_sleep
        return False


class TestWanxSucceededWithoutImage(unittest.TestCase):
    def test_succeeded_empty_result_exits_immediately(self):
        module = load_module()
        with WanxHarness(module, {"task_status": "SUCCEEDED", "results": [{}]}) as h:
            with self.assertRaises(SystemExit) as ctx:
                module._wanx("a cat", "wanx2.1-t2i-turbo", "1024*1024", "sk-x")
        self.assertEqual(ctx.exception.code, 1)
        self.assertLessEqual(h.polls, 2, "must stop on the first SUCCEEDED poll")

    def test_succeeded_with_url_returns_url(self):
        module = load_module()
        payload = {"task_status": "SUCCEEDED",
                   "results": [{"url": "https://example.com/x.png"}]}
        with WanxHarness(module, payload) as h:
            src = module._wanx("a cat", "wanx2.1-t2i-turbo", "1024*1024", "sk-x")
        self.assertEqual(src, "https://example.com/x.png")
        self.assertLessEqual(h.polls, 2)

    def test_succeeded_with_b64_returns_b64(self):
        module = load_module()
        payload = {"task_status": "SUCCEEDED",
                   "results": [{"b64_image": "aGVsbG8="}]}
        with WanxHarness(module, payload) as h:
            src = module._wanx("a cat", "wanx2.1-t2i-turbo", "1024*1024", "sk-x")
        self.assertEqual(src, "b64:aGVsbG8=")
        self.assertLessEqual(h.polls, 2)

    def test_failed_task_exits_with_message(self):
        module = load_module()
        payload = {"task_status": "FAILED", "message": "quota exceeded"}
        with WanxHarness(module, payload):
            with self.assertRaises(SystemExit) as ctx:
                module._wanx("a cat", "wanx2.1-t2i-turbo", "1024*1024", "sk-x")
        self.assertEqual(ctx.exception.code, 1)


if __name__ == "__main__":
    unittest.main()
