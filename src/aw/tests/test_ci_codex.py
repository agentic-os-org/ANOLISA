"""Failure cleanup of the pinned Codex app-server acceptance process."""

import os
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


if __name__ == "__main__":
    unittest.main()
