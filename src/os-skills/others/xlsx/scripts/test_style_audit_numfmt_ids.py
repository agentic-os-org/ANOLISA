#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for style_audit.py custom numFmt id classification.

Regression tests for the hardcoded CUSTOM numFmt ids (>= 164) that used to
sit in PERCENT_FMT_IDS/COMMA_FMT_IDS: a workbook that declares id 167 as
formatCode "0" got a false year_with_comma_format violation (exit 1 on a
compliant file), and id 165 as "0.00" produced a false percent warning.
Custom ids must be classified via their declared formatCode only; the
built-in ids and the formatCode fallback keep catching real offenders.
"""

import os
import subprocess
import sys
import tempfile
import unittest
import zipfile

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "style_audit.py")

STYLES_TEMPLATE = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="{ns}">
  <numFmts count="1"><numFmt numFmtId="{fmt_id}" formatCode="{fmt_code}"/></numFmts>
  <fonts count="2">
    <font><sz val="11"/><color theme="1"/></font>
    <font><sz val="11"/><color rgb="000000FF"/></font>
  </fonts>
  <fills count="2">
    <fill><patternFill patternType="none"/></fill>
    <fill><patternFill patternType="gray125"/></fill>
  </fills>
  <borders count="1"><border/></borders>
  <cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
  <cellXfs count="2">
    <xf numFmtId="0" fontId="0" fillId="0" borderId="0"/>
    <xf numFmtId="{fmt_id}" fontId="1" fillId="0" borderId="0" applyNumberFormat="1"/>
  </cellXfs>
</styleSheet>
"""

# numFmtId not declared under <numFmts> (built-in), so numFmts is omitted.
STYLES_BUILTIN = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="{ns}">
  <fonts count="2">
    <font><sz val="11"/><color theme="1"/></font>
    <font><sz val="11"/><color rgb="000000FF"/></font>
  </fonts>
  <fills count="2">
    <fill><patternFill patternType="none"/></fill>
    <fill><patternFill patternType="gray125"/></fill>
  </fills>
  <borders count="1"><border/></borders>
  <cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
  <cellXfs count="2">
    <xf numFmtId="0" fontId="0" fillId="0" borderId="0"/>
    <xf numFmtId="{fmt_id}" fontId="1" fillId="0" borderId="0" applyNumberFormat="1"/>
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

SHEET_TEMPLATE = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <sheetData><row r="1"><c r="A1" s="1"><v>{{value}}</v></c></row></sheetData>
</worksheet>
"""


def build_xlsx(path, styles_xml, value):
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
        z.writestr("xl/styles.xml", styles_xml)
        z.writestr("xl/worksheets/sheet1.xml", SHEET_TEMPLATE.format(value=value))
    return path


def run_audit(path):
    return subprocess.run([sys.executable, SCRIPT, path],
                          capture_output=True, text=True)


class TestCustomNumFmtIds(unittest.TestCase):
    def _build(self, root, name, template, fmt_id, fmt_code, value):
        styles = template.format(ns=NS_MAIN, fmt_id=fmt_id, fmt_code=fmt_code)
        return build_xlsx(os.path.join(root, name), styles, value)

    def test_custom_167_plain_zero_year_is_compliant(self):
        """Custom id 167 declared as "0" must not be treated as comma format."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = self._build(root, "a.xlsx", STYLES_TEMPLATE, 167, "0", "2024")
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 0, result.stdout)
            self.assertNotIn("uses comma-format", result.stdout)
            self.assertIn("PASS", result.stdout)

    def test_custom_165_decimal_is_not_percent(self):
        """Custom id 165 declared as "0.00" must not raise a percent warning."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = self._build(root, "b.xlsx", STYLES_TEMPLATE, 165, "0.00", "2024.5")
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 0, result.stdout)
            self.assertNotIn("percent-format cell", result.stdout)
            self.assertNotIn("202450%", result.stdout)

    def test_control_custom_200_decimal_still_clean(self):
        """Control: an id outside the built-in sets was already handled right."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = self._build(root, "c.xlsx", STYLES_TEMPLATE, 200, "0.00", "2024.5")
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 0, result.stdout)

    def test_builtin_comma_year_still_flagged(self):
        """Built-in id 3 (#,##0) with a year keeps failing (no regression)."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = self._build(root, "d.xlsx", STYLES_BUILTIN, 3, "#,##0", "2024")
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 1, result.stdout)
            self.assertIn("uses comma-format", result.stdout)

    def test_custom_comma_year_flagged_via_format_code(self):
        """A custom comma format is still caught through its formatCode."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = self._build(root, "e.xlsx", STYLES_TEMPLATE, 210, "#,##0", "2024")
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 1, result.stdout)
            self.assertIn("uses comma-format", result.stdout)

    def test_builtin_percent_still_warns(self):
        """Built-in id 9 (0%) with value 8 keeps warning (no regression)."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = self._build(root, "f.xlsx", STYLES_BUILTIN, 9, "0%", "8")
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 0, result.stdout)
            self.assertIn("percent-format cell", result.stdout)

    def test_custom_percent_still_warns_via_format_code(self):
        """A custom percent format is still caught through its formatCode."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = self._build(root, "g.xlsx", STYLES_TEMPLATE, 211, "0.0%", "8")
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 0, result.stdout)
            self.assertIn("percent-format cell", result.stdout)


if __name__ == "__main__":
    unittest.main()
