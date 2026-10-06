#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_shift_rows.py formula shifting.

Regression tests for digit-suffixed function names (LOG10, ATAN2, DAYS360,
BIN2DEC) and scientific-notation literals, which the cell-reference regex
used to rewrite as if they were cell addresses.
"""

import os
import sys
import unittest

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
if SCRIPTS_DIR not in sys.path:
    sys.path.insert(0, SCRIPTS_DIR)

from xlsx_shift_rows import shift_formula  # noqa: E402


class TestShiftFormulaFunctionNames(unittest.TestCase):
    def test_log10_name_is_preserved_while_arguments_shift(self):
        self.assertEqual(shift_formula("LOG10(B2)", at=2, delta=2), "LOG10(B4)")

    def test_atan2_name_is_preserved(self):
        self.assertEqual(shift_formula("ATAN2(B2,C3)", at=2, delta=2), "ATAN2(B4,C5)")

    def test_days360_name_is_preserved(self):
        self.assertEqual(shift_formula("DAYS360(B2,C3)", at=2, delta=2), "DAYS360(B4,C5)")

    def test_base_conversion_function_name_is_preserved(self):
        self.assertEqual(shift_formula("BIN2DEC(B2)", at=2, delta=2), "BIN2DEC(B4)")

    def test_scientific_notation_literal_is_preserved(self):
        self.assertEqual(shift_formula("=B2*1E5", at=2, delta=2), "=B4*1E5")

    def test_plain_cell_references_still_shift(self):
        self.assertEqual(shift_formula("SUM($B$7:$B$9)", at=5, delta=2), "SUM($B$9:$B$11)")
        self.assertEqual(shift_formula("B7+C8", at=7, delta=1), "B8+C9")

    def test_rows_below_insertion_point_are_untouched(self):
        self.assertEqual(shift_formula("B1+C2", at=5, delta=2), "B1+C2")


if __name__ == "__main__":
    unittest.main()
