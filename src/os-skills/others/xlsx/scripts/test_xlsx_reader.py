#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_reader.py CSV loading.

Regression tests for the encoding-fallback loop: only UnicodeDecodeError
may trigger the next encoding. The loop used to catch every Exception, so
a structurally broken CSV (ragged rows) was retried under all four
encodings and finally reported as "Cannot decode" — with latin-1 last in
the list, which never fails to decode, guaranteeing the misdiagnosis.
"""

import os
import subprocess
import sys
import tempfile
import unittest

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "xlsx_reader.py")


class XlsxReaderCsvLoadingTest(unittest.TestCase):

    def run_reader(self, path, *flags):
        return subprocess.run(
            [sys.executable, SCRIPT, path, *flags],
            capture_output=True,
            text=True,
        )

    def test_ragged_csv_reports_the_parse_error_not_a_decode_failure(self):
        # A CSV whose third row has more fields than the header is a structural
        # error, not an encoding problem; the message must name the real cause.
        with tempfile.NamedTemporaryFile(
            "w", suffix=".csv", delete=False, encoding="utf-8"
        ) as handle:
            handle.write("a,b\n1,2\n3,4,5\n")
            path = handle.name
        try:
            result = self.run_reader(path, "--quality")
            self.assertNotEqual(result.returncode, 0, result.stderr)
            self.assertIn("ERROR:", result.stderr)
            self.assertNotIn("Cannot decode", result.stderr)
            self.assertIn("tokenizing", result.stderr)
        finally:
            os.unlink(path)

    def test_gbk_csv_still_loads_through_the_fallback(self):
        # Chinese text saved as GBK must keep loading: utf-8-sig fails with
        # UnicodeDecodeError and the loop falls through to gbk.
        with tempfile.NamedTemporaryFile(
            "wb", suffix=".csv", delete=False
        ) as handle:
            handle.write("名称,数量\n销售额,42\n".encode("gbk"))
            path = handle.name
        try:
            result = self.run_reader(path, "--json")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("销售额", result.stdout)
        finally:
            os.unlink(path)

    def test_bom_csv_loads(self):
        with tempfile.NamedTemporaryFile(
            "wb", suffix=".csv", delete=False
        ) as handle:
            handle.write("a,b\n1,2\n".encode("utf-8-sig"))
            path = handle.name
        try:
            result = self.run_reader(path, "--quality")
            self.assertEqual(result.returncode, 0, result.stderr)
        finally:
            os.unlink(path)


if __name__ == "__main__":
    unittest.main()
