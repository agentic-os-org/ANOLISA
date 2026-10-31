#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_reader.py --json on sparse numeric columns.

Regression test for #3963: `compute_stats` handed pandas' `describe()` output
straight to `json.dumps`, and `default=str` never fires for floats — so a
numeric column with fewer than two values put a bare `NaN` token into the
`--json` document. RFC 8259 has no NaN, and strict parsers (Node, Go, Rust)
reject the whole document, exactly when the data is sparse.

The counter-case matters as much as the fix: a column that *can* have a std must
keep it, so the sanitizer has to be about "not a finite number", not about
"values we do not like".

Usage: python3 src/os-skills/others/xlsx/scripts/test_xlsx_reader_json_nan.py
"""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("xlsx_reader.py")


try:
    import pandas  # noqa: F401  (xlsx_reader itself requires pandas)

    HAS_PANDAS = True
except ImportError:
    HAS_PANDAS = False


def reject_constant(token):
    raise AssertionError(f"the document must not contain {token} (RFC 8259 forbids it)")


def strict_loads(text):
    return json.loads(text, parse_constant=reject_constant)


@unittest.skipUnless(
    HAS_PANDAS and sys.version_info >= (3, 10),
    "xlsx_reader.py needs pandas and Python >= 3.10 (PEP 604 annotations are evaluated at def time)",
)
class XlsxReaderJsonNanTests(unittest.TestCase):
    def run_tool(self, csv_text, *args):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "data.csv"
            path.write_text(csv_text, encoding="utf-8")
            return subprocess.run(
                [sys.executable, str(SCRIPT), str(path), *args],
                capture_output=True,
                text=True,
            )

    @staticmethod
    def first_column_stats(document):
        sheet = next(iter(document["stats"].values()))
        return next(iter(sheet.values()))

    def test_a_single_value_column_serializes_std_as_null(self):
        result = self.run_tool("num\n5\n", "--json")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("NaN", result.stdout, "a bare NaN token is not JSON")
        self.assertNotIn("Infinity", result.stdout)

        column = self.first_column_stats(strict_loads(result.stdout))
        self.assertIsNone(column["std"], "a std that cannot be computed is null, not NaN")
        self.assertEqual(column["count"], 1, "the computable statistics must stay")
        self.assertEqual(column["min"], 5)

    def test_a_missing_cell_in_the_structure_preview_serializes_as_null(self):
        # Any null in the first five rows of any column reaches
        # structure.preview, which a sanitizer attached to compute_stats never
        # covered — the more common shape of the same invalid document.
        fixture = chr(10).join(["name,num", "alice,5", "bob,", "carol,7", ",9", ""])
        result = self.run_tool(fixture, "--json")
        self.assertNotIn("NaN", result.stdout, "the preview must not leak a NaN token")

        document = strict_loads(result.stdout)
        preview = document["structure"]["data"]["preview"]
        self.assertIsNone(preview[1]["num"], "a missing numeric cell is null")
        self.assertIsNone(preview[3]["name"], "a missing text cell is null too")

    def test_a_column_with_two_values_keeps_its_std(self):
        result = self.run_tool("num\n5\n7\n", "--json")
        self.assertEqual(result.returncode, 0, result.stderr)
        column = self.first_column_stats(strict_loads(result.stdout))
        self.assertIsNotNone(column["std"])
        self.assertAlmostEqual(column["std"], 1.4142, places=3)

    def test_the_human_readable_report_survives_a_sparse_column(self):
        result = self.run_tool("num\n5\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Numeric column statistics", result.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2)
