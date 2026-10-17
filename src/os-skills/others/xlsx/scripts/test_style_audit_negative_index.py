#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for style_audit.py style index range checking.

Regression tests for negative `s` attributes: the out-of-range check
only tested the upper bound, so s="-1" silently wrapped to the last
xf via Python negative indexing (auditing the cell against the wrong
style), and s="-5" crashed the tool with an uncaught IndexError
instead of producing a structured violation report.
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import style_audit  # noqa: E402

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"

# 2 fonts (black theme font, blue font), 2 xfs (normal, blue)
STYLES_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="{NS_MAIN}">
  <fonts count="2">
    <font><sz val="11"/><color theme="1"/></font>
    <font><sz val="11"/><color rgb="000000FF"/></font>
  </fonts>
  <fills count="2">
    <fill><patternFill patternType="none"/></fill>
    <fill><patternFill patternType="gray125"/></fill>
  </fills>
  <borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders>
  <cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
  <cellXfs count="2">
    <xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>
    <xf numFmtId="0" fontId="1" fillId="0" borderId="0" xfId="0" applyFont="1"/>
  </cellXfs>
</styleSheet>
"""


def _sheet_xml(s_attr):
    return (f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
            f'<worksheet xmlns="{NS_MAIN}"><sheetData>'
            f'<row r="1"><c r="A1" s="{s_attr}"><f>1+1</f><v>2</v></c></row>'
            f'</sheetData></worksheet>')


class TestNegativeStyleIndex(unittest.TestCase):
    def _audit(self, s_attr):
        return style_audit._audit(STYLES_XML.encode(), [("Sheet1", _sheet_xml(s_attr).encode())])

    def test_negative_beyond_range_reports_violation(self):
        """s="-5" must be reported as out of range, not crash with IndexError."""
        result = self._audit(-5)
        types = [v["type"] for v in result["violations"]]
        self.assertIn("style_index_out_of_range", types,
                      "negative index beyond range must be reported")
        hit = next(v for v in result["violations"] if v["type"] == "style_index_out_of_range")
        self.assertEqual(hit["s"], -5)

    def test_negative_one_does_not_wrap(self):
        """s="-1" must be reported as out of range, not silently audited as xfs[-1]."""
        result = self._audit(-1)
        types = [v["type"] for v in result["violations"]]
        self.assertIn("style_index_out_of_range", types,
                      "s=-1 must not silently wrap to the last xf")

    def test_valid_index_still_audits(self):
        """正常链路保护：s=1（蓝字 xf）的公式单元格仍被抓到 color-role 违规。"""
        result = self._audit(1)
        types = [v["type"] for v in result["violations"]]
        self.assertIn("formula_cell_blue_font", types)

    def test_positive_out_of_range_still_reported(self):
        """既有行为保护：正数越界照报。"""
        result = self._audit(2)
        types = [v["type"] for v in result["violations"]]
        self.assertIn("style_index_out_of_range", types)


if __name__ == "__main__":
    unittest.main()
