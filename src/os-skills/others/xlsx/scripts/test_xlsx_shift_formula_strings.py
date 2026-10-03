#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_shift_rows.py shift_formula string-literal handling.

Regression tests for double-quoted string literals inside formulas:
_shift_refs rewrote every uppercase-letter-run+digits token it saw, so
string content like "FY2025" or "Q1 review" was corrupted into
"FY2026" / "Q3 review" when rows were inserted or deleted.
"""

import os
import sys
import unittest

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, SCRIPTS_DIR)

from xlsx_shift_rows import shift_formula  # noqa: E402


class TestShiftFormulaStrings(unittest.TestCase):
    def test_string_literal_not_shifted(self):
        # at=3, delta=1 shifts B12 -> B13 but must leave "FY2025" alone.
        self.assertEqual(
            shift_formula('IF(B12="FY2025","budget","other")', at=3, delta=1),
            'IF(B13="FY2025","budget","other")',
        )

    def test_string_literal_with_ref_shaped_text_not_shifted(self):
        self.assertEqual(
            shift_formula('IF(B2="Q1 review","go","no")', at=1, delta=2),
            'IF(B4="Q1 review","go","no")',
        )

    def test_escaped_quote_inside_string_not_shifted(self):
        # Excel doubles quotes inside string literals.
        self.assertEqual(
            shift_formula('IF(B2="cell ""A1"" is here","x","y")', at=2, delta=1),
            'IF(B3="cell ""A1"" is here","x","y")',
        )

    def test_real_refs_still_shift(self):
        self.assertEqual(shift_formula("B7+$B$8", at=5, delta=1), "B8+$B$9")
        self.assertEqual(
            shift_formula('CONCATENATE(A2,"FY2025")', at=1, delta=1),
            'CONCATENATE(A3,"FY2025")',
        )


if __name__ == "__main__":
    unittest.main()
