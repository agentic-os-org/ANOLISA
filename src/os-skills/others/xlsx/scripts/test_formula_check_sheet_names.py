#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for formula_check.py quoted sheet-name parsing.

Regression tests for sheet names containing apostrophes: Excel escapes
an apostrophe in a sheet name by doubling it inside the quotes, so a
formula referencing the sheet It's is written 'It''s'!A1. The quoted-
name pattern used to stop at the first apostrophe and capture "s",
producing a false broken_sheet_ref against a sheet that does exist.
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import formula_check as fc  # noqa: E402


class TestExtractSheetRefs(unittest.TestCase):
    def test_quoted_name_with_escaped_apostrophe(self):
        """'It''s'!A1 references the sheet named It's — not a sheet named s.

        Before the fix the pattern '([^']+)'! captured "s", so the
        validator reported a missing sheet "s" for a perfectly valid
        formula.
        """
        self.assertEqual(fc.extract_sheet_refs("SUM('It''s'!A1:B2)"), ["It's"])

    def test_escaped_apostrophe_mixed_with_plain_quoted(self):
        self.assertEqual(
            fc.extract_sheet_refs("='Q1 Data'!A1+'It''s'!B2"),
            ["Q1 Data", "It's"],
        )

    def test_name_that_is_only_an_escaped_apostrophe(self):
        # A sheet literally named ' — quoted as '''' (quote, doubled, quote).
        self.assertEqual(fc.extract_sheet_refs("=''''!A1"), ["'"])

    def test_plain_quoted_name_unchanged(self):
        self.assertEqual(fc.extract_sheet_refs("='Budget FY25'!A1"), ["Budget FY25"])

    def test_unquoted_name_unchanged(self):
        self.assertEqual(fc.extract_sheet_refs("=Sheet1!A1"), ["Sheet1"])


if __name__ == "__main__":
    unittest.main()
