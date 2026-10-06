#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_reader.py CSV loading.

Regression test: an empty (0-byte) .csv is a valid, empty sheet, but the
encoding-fallback loop caught pandas' EmptyDataError as a decode failure
and aborted with "Cannot decode ... Tried encodings".
"""

import os
import sys
import tempfile
import unittest

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
if SCRIPTS_DIR not in sys.path:
    sys.path.insert(0, SCRIPTS_DIR)

from xlsx_reader import detect_and_load, explore_structure  # noqa: E402


class TestEmptyCsv(unittest.TestCase):
    def test_empty_csv_loads_as_empty_sheet(self):
        fd, path = tempfile.mkstemp(suffix=".csv")
        os.close(fd)  # leave the file empty
        self.addCleanup(os.unlink, path)
        try:
            sheets = detect_and_load(path)
        except Exception as exc:  # noqa: BLE001
            self.fail(f"detect_and_load raised {type(exc).__name__}: {exc}")
        stem = os.path.splitext(os.path.basename(path))[0]
        self.assertIn(stem, sheets)
        structure = explore_structure(sheets)
        for info in structure.values():
            self.assertEqual(info["shape"], {"rows": 0, "cols": 0})


if __name__ == "__main__":
    unittest.main()
