#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for style_audit.py numFmt rule false positives.

Regression tests for two misdetections in the financial-format audit:

1. A quoted "%" literal suffix (formatCode like 0.0"%") displays the value
   as-is with a percent sign — it does NOT scale by 100 — yet the percent
   rule matched any "%" in the formatCode and warned the author to store
   8 as 0.08, which with that format would display as 0.08% instead of 8%.
2. Conditional K/M-scaled formats ([>=1000]#,##0,"K";#,##0) slipped past
   the whole-code endswith exclusions: a year-like value 2024 renders as
   "2K" (the section that applies for >=1000 scales), never as "2,024",
   but the audit reported a comma-format corruption.
"""

import os
import subprocess
import sys
import tempfile
import unittest
import zipfile
from xml.sax.saxutils import escape

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "style_audit.py")


def _styles_xml(fmt_code: str) -> str:
    # font 0 is theme-colored (not blue/black rgb) so the color rules stay
    # quiet; xf 1 applies custom numFmt 164 to the value cell.
    return f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="{NS_MAIN}">
  <numFmts count="1"><numFmt numFmtId="164" formatCode="{escape(fmt_code, {'"': "&quot;"})}"/></numFmts>
  <fonts count="1"><font><sz val="11"/><color theme="1"/></font></fonts>
  <fills count="2">
    <fill><patternFill patternType="none"/></fill>
    <fill><patternFill patternType="gray125"/></fill>
  </fills>
  <borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders>
  <cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
  <cellXfs count="2">
    <xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>
    <xf numFmtId="164" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  </cellXfs>
</styleSheet>
"""


WORKBOOK_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL}">
  <sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets>
</workbook>
"""

RELS_XML = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
    Target="worksheets/sheet1.xml"/>
</Relationships>
"""


def _worksheet_xml(cell_value: str) -> str:
    return f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <sheetData>
    <row r="1"><c r="A1" s="1"><v>{cell_value}</v></c></row>
  </sheetData>
</worksheet>
"""


def build_xlsx(path: str, fmt_code: str, cell_value: str) -> str:
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("[Content_Types].xml",
                   '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
                   '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
                   '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
                   '<Default Extension="xml" ContentType="application/xml"/>'
                   '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
                   '</Types>')
        z.writestr("xl/workbook.xml", WORKBOOK_XML)
        z.writestr("xl/_rels/workbook.xml.rels", RELS_XML)
        z.writestr("xl/styles.xml", _styles_xml(fmt_code))
        z.writestr("xl/worksheets/sheet1.xml", _worksheet_xml(cell_value))
    return path


def run_audit(path: str) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, SCRIPT, path], capture_output=True, text=True)


class TestPercentLiteralSuffix(unittest.TestCase):
    def test_quoted_percent_suffix_not_flagged(self):
        """0.0"%" 显示 8 为 8.0%（不乘 100）。修复前规则按子串匹配 "%"
        误报警告并建议改存 0.08——按建议改后反而显示 0.08%。
        """
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "pct.xlsx"), '0.0"%"', "8")
            result = run_audit(xlsx)
            self.assertNotIn("percent-format cell has value=8", result.stdout, result.stdout)
            self.assertNotIn("likely should be stored as decimal", result.stdout, result.stdout)

    def test_quoted_percent_section_variant_not_flagged(self):
        """正负两段都是引号 % 字面量时同样不得误报。"""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "pct2.xlsx"), '0"%";-0"%"', "5")
            result = run_audit(xlsx)
            self.assertNotIn("percent-format cell has value=5", result.stdout, result.stdout)

    def test_true_percent_format_still_flagged(self):
        """正常链路保护：裸 % 格式（乘 100）值 >1 仍要警告。"""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "pct3.xlsx"), "0.0%", "8")
            result = run_audit(xlsx)
            self.assertIn("percent-format cell has value=8", result.stdout, result.stdout)
            self.assertIn("displays as 800%", result.stdout, result.stdout)


class TestConditionalKmScaledYear(unittest.TestCase):
    def test_conditional_k_scaled_year_not_flagged(self):
        """[>=1000]#,##0,"K";#,##0 对 2024 走第一段显示 "2K"（缩放），
        不会出现 2,024 的分组损坏。修复前整码 endswith 排除法漏掉多段
        条件格式，误报 comma-format。
        """
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "year.xlsx"),
                              '[>=1000]#,##0,"K";#,##0', "2024")
            result = run_audit(xlsx)
            self.assertNotIn("uses comma-format", result.stdout, result.stdout)
            self.assertNotIn("year value 2024", result.stdout, result.stdout)

    def test_plain_grouping_year_still_flagged(self):
        """正常链路保护：#,##0 对年份的 2,024 分组损坏仍要报。"""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "year2.xlsx"), "#,##0", "2024")
            result = run_audit(xlsx)
            self.assertIn("uses comma-format", result.stdout, result.stdout)
            self.assertIn("2,024", result.stdout, result.stdout)

    def test_whole_code_km_suffix_exclusion_kept(self):
        """既有排除保持：单段 #,##0,"K" 不报。"""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "year3.xlsx"), '#,##0,"K"', "2024")
            result = run_audit(xlsx)
            self.assertNotIn("uses comma-format", result.stdout, result.stdout)

    def test_conditional_fallthrough_to_grouping_flags(self):
        """条件段不命中时落到普通段：[<100]0;#,##0 对 2024 命中 #,##0
        分组段——必须仍报（守卫不得矫枉过正）。
        """
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "year4.xlsx"),
                              "[<100]0;#,##0", "2024")
            result = run_audit(xlsx)
            self.assertIn("uses comma-format", result.stdout, result.stdout)


if __name__ == "__main__":
    unittest.main()
