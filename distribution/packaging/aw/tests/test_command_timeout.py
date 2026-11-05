"""Verify the per-command timeout is configurable without editing the script."""

from __future__ import annotations

import os
import sys
import time
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import common

SLOW = [sys.executable, "-c", "import time; time.sleep(5)"]


class CommandTimeoutTest(unittest.TestCase):
    def test_default_applies_without_an_override(self) -> None:
        with patch.dict(os.environ, {}, clear=False):
            os.environ.pop("AW_BUILD_TIMEOUT", None)
            self.assertEqual(common.command_timeout(), 1200)

    def test_override_is_read_from_the_environment(self) -> None:
        with patch.dict(os.environ, {"AW_BUILD_TIMEOUT": "5400"}):
            self.assertEqual(common.command_timeout(), 5400)

    def test_blank_override_keeps_the_default(self) -> None:
        with patch.dict(os.environ, {"AW_BUILD_TIMEOUT": "  "}):
            self.assertEqual(common.command_timeout(), 1200)

    def test_non_numeric_override_names_the_variable(self) -> None:
        with patch.dict(os.environ, {"AW_BUILD_TIMEOUT": "soon"}):
            with self.assertRaises(RuntimeError) as caught:
                common.command_timeout()
            self.assertIn("AW_BUILD_TIMEOUT", str(caught.exception))

    def test_non_positive_override_is_rejected(self) -> None:
        for value in ("0", "-5"):
            with patch.dict(os.environ, {"AW_BUILD_TIMEOUT": value}):
                with self.assertRaises(RuntimeError):
                    common.command_timeout()

    def test_run_uses_the_override_when_no_timeout_is_passed(self) -> None:
        with patch.dict(os.environ, {"AW_BUILD_TIMEOUT": "1"}):
            started = time.monotonic()
            with self.assertRaises(RuntimeError) as caught:
                common.run(SLOW, Path.cwd(), common.Cancellation())
            self.assertIn("command exceeded 1s", str(caught.exception))
            self.assertIn("AW_BUILD_TIMEOUT", str(caught.exception))
            self.assertLess(time.monotonic() - started, 4)

    def test_an_explicit_timeout_still_wins(self) -> None:
        with patch.dict(os.environ, {"AW_BUILD_TIMEOUT": "600"}):
            with self.assertRaises(RuntimeError) as caught:
                common.run(SLOW, Path.cwd(), common.Cancellation(), 1)
            self.assertIn("command exceeded 1s", str(caught.exception))


if __name__ == "__main__":
    unittest.main()
