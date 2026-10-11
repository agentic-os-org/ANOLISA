#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for formula_check.py named-range candidate extraction.

Regression tests for sheet-name false positives: the cleanup patterns
only stripped sheet prefixes followed by an uppercase A1-style
reference, so a 3D range (Sheet1:Sheet2!A1) left "Sheet1" behind and a
sheet-qualified name (Sheet1!myrate) left both "Sheet1" and "myrate"
behind — each then reported as unknown_name_ref against names that
exist (as sheets or as sheet-scoped names).
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import formula_check as fc  # noqa: E402


class TestExtractNameRefs(unittest.TestCase):
    def test_3d_range_leaves_no_candidates(self):
        """SUM(Sheet1:Sheet2!A1) references sheets, not names.

        Before the fix the strip removed only "Sheet2!A1", leaving
        "Sheet1" as a candidate and a false unknown_name_ref warning.
        """
        self.assertEqual(fc.extract_name_refs("=SUM(Sheet1:Sheet2!A1)"), [])

    def test_sheet_qualified_name_leaves_no_candidates(self):
        """Sheet1!myrate is a sheet-scoped reference, not a workbook name."""
        self.assertEqual(fc.extract_name_refs("=Sheet1!myrate*2"), [])

    def test_bare_name_still_extracted(self):
        """Control: an unqualified name is still a candidate."""
        self.assertEqual(fc.extract_name_refs("=myrate*2"), ["myrate"])

    def test_quoted_sheet_ref_still_stripped(self):
        self.assertEqual(
            fc.extract_name_refs("='Q1 Data'!A1+'It''s'!B2"), []
        )

    def test_mixed_formula_keeps_real_name(self):
        """A real name next to a 3D range is still reported."""
        self.assertEqual(
            fc.extract_name_refs("=TOTALS*SUM(Sheet1:Sheet2!A1)"), ["TOTALS"]
        )


if __name__ == "__main__":
    unittest.main()
