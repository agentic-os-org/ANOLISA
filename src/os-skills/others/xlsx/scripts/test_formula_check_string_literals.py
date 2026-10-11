#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for formula_check.py string-literal handling in reference scans.

Regression tests for double-quoted string literals: the sheet-reference
and named-range scanners matched identifiers inside "..." literals, so a
formula like =IF(B1>1,"Over budget!","OK") produced a false
broken_sheet_ref for a sheet named "budget" plus false unknown_name_ref
warnings - a blocking FAIL (exit 1) for a perfectly valid formula.
External workbook references ([1]Prices!A1) were also flagged against
the local sheet list.
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import formula_check as fc  # noqa: E402


class TestStringLiteralsIgnored(unittest.TestCase):
    def test_string_literal_with_exclamation_not_a_sheet_ref(self):
        """="Over budget!" 的字符串内容不得被当作 sheet 引用。"""
        self.assertEqual(
            fc.extract_sheet_refs('=IF(B1>1,"Over budget!","OK")'), []
        )

    def test_chinese_string_literal_ignored(self):
        self.assertEqual(fc.extract_sheet_refs('="预算超标!"'), [])

    def test_external_workbook_ref_not_checked_against_local_sheets(self):
        """[1]Prices!A1 是外部工作簿引用，不得按本地 sheet 清单判 missing。"""
        self.assertEqual(fc.extract_sheet_refs("=[1]Prices!A1"), [])

    def test_real_sheet_ref_still_extracted(self):
        """正常链路保护：真实 sheet 引用照常提取。"""
        self.assertEqual(fc.extract_sheet_refs('=Sheet1!A1+"x"'), ["Sheet1"])
        self.assertEqual(
            fc.extract_sheet_refs("=SUM('Q1 Data'!A1)"), ["Q1 Data"]
        )

    def test_sheet_ref_next_to_literal_still_found(self):
        self.assertEqual(
            fc.extract_sheet_refs('="plain" + Sheet1!A1'), ["Sheet1"]
        )

    def test_name_refs_skip_string_literals(self):
        """named-range 扫描同样跳过字符串内容（对照侧）。"""
        self.assertEqual(
            fc.extract_name_refs('=IF(B1>1,"Over budget!","OK")'), []
        )
        self.assertEqual(fc.extract_name_refs("=myrate*2"), ["myrate"])


if __name__ == "__main__":
    unittest.main()
