#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for style_audit.py numFmts count integrity.

Regression tests for Check A skipping numFmts: <numFmts count="9"> with a
single child used to pass the integrity audit silently although _parse_styles
already computed num_fmts_declared / num_fmts_actual.
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
  <numFmts count="{declared}"><numFmt numFmtId="164" formatCode="0.00"/></numFmts>
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
    <xf numFmtId="164" fontId="1" fillId="0" borderId="0" applyNumberFormat="1"/>
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

WORKSHEET_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <sheetData><row r="1"><c r="A1" s="1"><v>2024.5</v></c></row></sheetData>
</worksheet>
"""


def build_xlsx(path, declared):
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
        z.writestr("xl/styles.xml", STYLES_TEMPLATE.format(ns=NS_MAIN, declared=declared))
        z.writestr("xl/worksheets/sheet1.xml", WORKSHEET_XML)
    return path


def run_audit(path):
    return subprocess.run([sys.executable, SCRIPT, path],
                          capture_output=True, text=True)


class TestNumFmtsCountIntegrity(unittest.TestCase):
    def test_declared_count_mismatch_is_flagged(self):
        """<numFmts count="9"> with one child must produce a violation."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "bad.xlsx"), declared=9)
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 1, result.stdout)
            self.assertIn("numFmts count mismatch", result.stdout)
            self.assertIn("declared=9", result.stdout)
            self.assertIn("actual=1", result.stdout)

    def test_consistent_count_still_passes(self):
        """Control: count="1" with one child stays clean."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "good.xlsx"), declared=1)
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 0, result.stdout)
            self.assertIn("PASS", result.stdout)
            self.assertNotIn("count mismatch", result.stdout)

    def test_json_report_carries_violation(self):
        """--json output includes the structured numFmts count_mismatch."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "bad.xlsx"), declared=9)
            result = subprocess.run([sys.executable, SCRIPT, xlsx, "--json"],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 1, result.stdout)
            self.assertIn('"element": "numFmts"', result.stdout)
            self.assertIn('"declared": 9', result.stdout)
            self.assertIn('"actual": 1', result.stdout)


if __name__ == "__main__":
    unittest.main()
