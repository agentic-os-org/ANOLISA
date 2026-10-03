#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_reader.py --json output validity.

describe() yields NaN whenever a statistic is not computable (std of a
single value); json.dumps(default=str) never fires for floats, so the
output used to contain a bare NaN token — invalid JSON per RFC 8259 and
rejected by strict parsers in Node, Go and Rust.
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest

try:
    import pandas  # noqa: F401  (availability guard only)

    PANDAS_AVAILABLE = True
except ImportError:
    PANDAS_AVAILABLE = False

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "xlsx_reader.py")


def reject_constant(token: str):
    raise ValueError(f"invalid JSON constant: {token}")


def run_reader(args: list) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, SCRIPT] + args,
                          capture_output=True, text=True)


@unittest.skipIf(not PANDAS_AVAILABLE, "pandas not installed")
class TestJsonOutputIsStrictParseable(unittest.TestCase):
    def test_single_value_column_stats_parse_strictly(self):
        """One numeric value -> std is NaN; output must still be valid JSON."""
        with tempfile.TemporaryDirectory() as root:
            csv_path = os.path.join(root, "single.csv")
            with open(csv_path, "w", encoding="utf-8") as f:
                f.write("num\n5\n")
            result = run_reader([csv_path, "--json"])
            self.assertEqual(result.returncode, 0, result.stderr)
            # parse_constant fires on NaN/Infinity tokens: must not raise.
            payload = json.loads(result.stdout, parse_constant=reject_constant)
            col_stats = payload["stats"]["single"]["num"]
            self.assertEqual(col_stats["count"], 1.0)
            self.assertIsNone(col_stats["std"])
            self.assertEqual(col_stats["mean"], 5.0)

    def test_multi_value_column_still_has_std(self):
        """Healthy column: std remains a real number after the change."""
        with tempfile.TemporaryDirectory() as root:
            csv_path = os.path.join(root, "three.csv")
            with open(csv_path, "w", encoding="utf-8") as f:
                f.write("num\n1\n2\n3\n")
            result = run_reader([csv_path, "--json"])
            self.assertEqual(result.returncode, 0, result.stderr)
            payload = json.loads(result.stdout, parse_constant=reject_constant)
            self.assertEqual(payload["stats"]["three"]["num"]["count"], 3.0)
            self.assertIsInstance(payload["stats"]["three"]["num"]["std"], float)


if __name__ == "__main__":
    unittest.main()
