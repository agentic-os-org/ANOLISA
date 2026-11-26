"""Failure cleanup of the pinned Codex app-server acceptance process."""

import json
import os
from pathlib import Path
import runpy
import select
import signal
import subprocess
import sys
import unittest
from unittest.mock import patch

from test_codex_app_server import stop_child


class CodexCleanupTests(unittest.TestCase):
    def test_closed_input_and_ignored_interrupt_still_kill_and_reap(self):
        for failure in (BrokenPipeError("closed"), OSError("close failed")):
            with self.subTest(failure=type(failure).__name__):
                child = subprocess.Popen(
                    [sys.executable, "-c", "import signal,time; "
                     "signal.signal(signal.SIGINT, signal.SIG_IGN); "
                     "print('ready', flush=True); time.sleep(30)"],
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, start_new_session=True,
                )
                try:
                    self.assertTrue(select.select([child.stdout], [], [], 3)[0])
                    self.assertEqual(child.stdout.readline(), b"ready\n")
                    child.stdin.close()
                    with patch.object(child.stdin, "close", side_effect=failure):
                        if isinstance(failure, BrokenPipeError):
                            stop_child(child, timeout=0.05)
                        else:
                            with self.assertRaisesRegex(OSError, "close failed"):
                                stop_child(child, timeout=0.05)
                    self.assertEqual(child.returncode, -signal.SIGKILL)
                    self.assertTrue(child.stdout.closed)
                finally:
                    try:
                        os.killpg(child.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    child.wait(timeout=3)
                    child.stdin.close()
                    child.stdout.close()

    def test_thread_provider_is_pinned_to_the_launch(self):
        guard = Path(__file__).resolve().parents[1] / "crates/aw-adapter/native/codex/stdio.py"
        session = runpy.run_path(str(guard))["Session"]("local-v2_test")
        for request, params, expected in [
            (1, {}, True),
            (2, {"modelProvider": "local-v2_test"}, True),
            (3, {"modelProvider": "gateway"}, False),
            (4, {"modelProvider": None}, False),
        ]:
            self.assertEqual(session.admit({"id": request, "method": "thread/start", "params": params}), expected)

    def test_gateway_accepts_only_plain_text_items(self):
        guard = Path(__file__).resolve().parents[1] / "crates/aw-adapter/native/codex/stdio.py"
        session = runpy.run_path(str(guard))["Session"]("gateway")
        session.threads.add("worker")
        for request, (fields, expected) in enumerate([
            ({}, True), ({"text_elements": []}, True),
            ({"text_elements": [{}]}, False), ({"text_elements": None}, False),
            ({"extra": []}, False), ({"type": "image"}, False), ({"text": None}, False),
        ]):
            with self.subTest(fields=fields):
                item = {"type": "text", "text": "hello", **fields}
                params = {"threadId": "worker", "input": [item]}
                self.assertEqual(session.admit({"id": request, "method": "turn/start", "params": params}), expected)

    def test_gateway_tracks_string_and_integer_ids_separately(self):
        guard = Path(__file__).resolve().parents[1] / "crates/aw-adapter/native/codex/stdio.py"
        session = runpy.run_path(str(guard))["Session"]("gateway")
        for request in (1, "1"):
            self.assertTrue(session.admit({"id": request, "method": "initialize"}))
        for request in (True, 1.0, None, [], 1, "1"):
            with self.subTest(request=request), self.assertRaises(ValueError):
                session.admit({"id": request, "method": "initialize"})
        session.received({"id": "1", "result": {}})
        self.assertEqual(session.pending, {1: "initialize"})
        self.assertTrue(session.admit({"id": "1", "method": "initialize"}))

    def test_gateway_eof_waits_for_all_pending_replies(self):
        guard = Path(__file__).resolve().parents[1] / "crates/aw-adapter/native/codex/stdio.py"
        fixture = """
import json, select, sys
requests = [json.loads(sys.stdin.buffer.readline()) for _ in range(2)]
for request in reversed(requests):
    if select.select([sys.stdin.buffer], [], [], 0.1)[0]:
        break
    print(json.dumps({"id": request["id"], "result": {}}), flush=True)
assert sys.stdin.buffer.read() == b""
"""
        requests = b'{"id":1,"method":"initialize"}\n{"id":2,"method":"initialize"}\n'
        relay = subprocess.run([sys.executable, "-I", guard, "gateway", sys.executable, "-c", fixture],
                               input=requests, capture_output=True, timeout=8, check=True)
        self.assertEqual([json.loads(line)["id"] for line in relay.stdout.splitlines()], [2, 1])

    def test_gateway_shutdown_reaps_idle_native_child(self):
        guard = Path(__file__).resolve().parents[1] / "crates/aw-adapter/native/codex/stdio.py"
        fixture = ("import json,os,signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); "
                   "print(json.dumps({'method':'fixture','params':os.getpid()}),flush=True); time.sleep(30)")
        with subprocess.Popen([sys.executable, "-I", guard, "gateway", sys.executable, "-c", fixture],
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE, start_new_session=True) as relay:
            try:
                self.assertTrue(select.select([relay.stdout], [], [], 5)[0])
                pid = json.loads(relay.stdout.readline())["params"]
                relay.terminate()
                self.assertEqual(relay.wait(timeout=5), 128 + signal.SIGTERM)
                self.assertFalse(Path(f"/proc/{pid}").exists())
            finally:
                try:
                    os.killpg(relay.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass


if __name__ == "__main__":
    unittest.main()
