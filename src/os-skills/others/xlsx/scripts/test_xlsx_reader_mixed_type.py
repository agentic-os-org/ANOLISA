#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for the xlsx_reader.py mixed-type audit column selection.

select_dtypes(include="object") raised a Pandas4Warning on pandas 3 and
will stop matching str-dtype columns on pandas 4 — silently disabling
the mixed-type check for exactly the text columns it exists to audit.
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

# 'qty' mixes numbers stored as text with real text: 2 convertible, 1 not.
MIXED_CSV = "id,qty,note\n1,10,ok\n2,abc,hello\n3,30,ok\n"


def run_reader(args: list) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, SCRIPT] + args,
                          capture_output=True, text=True)


@unittest.skipIf(not PANDAS_AVAILABLE, "pandas not installed")
class TestMixedTypeAuditColumnSelection(unittest.TestCase):
    def test_mixed_type_finding_fires_without_pandas4_warning(self):
        """The audit finds the mixed column and stderr stays warning-free."""
        with tempfile.TemporaryDirectory() as root:
            csv_path = os.path.join(root, "mixed.csv")
            with open(csv_path, "w", encoding="utf-8") as f:
                f.write(MIXED_CSV)
            result = run_reader([csv_path, "--quality", "--json"])
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn("Pandas4Warning", result.stderr)
            self.assertNotIn("Warning", result.stderr)
            findings = json.loads(result.stdout)["quality"]["mixed"]
            mixed = [f for f in findings if f["type"] == "mixed_type"]
            self.assertEqual(len(mixed), 1, findings)
            self.assertEqual(mixed[0]["column"], "qty")
            self.assertEqual(mixed[0]["convertible_to_numeric"], 2)
            self.assertEqual(mixed[0]["non_convertible"], 1)

    def test_clean_numeric_column_no_mixed_type(self):
        """A purely numeric column must not raise a mixed-type finding."""
        with tempfile.TemporaryDirectory() as root:
            csv_path = os.path.join(root, "clean.csv")
            with open(csv_path, "w", encoding="utf-8") as f:
                f.write("id\n1\n2\n3\n")
            result = run_reader([csv_path, "--quality", "--json"])
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn("Pandas4Warning", result.stderr)
            findings = json.loads(result.stdout)["quality"]["clean"]
            self.assertEqual(
                [f for f in findings if f["type"] == "mixed_type"], [])


if __name__ == "__main__":
    unittest.main()
