#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for style_audit.py worksheet resolution.

Regression tests for package-absolute rel targets
(Target="/xl/worksheets/sheet1.xml"), which used to be mis-joined to
xl//xl/... so zero sheets were audited and the tool printed PASS, and
for failing loudly whenever zero sheets were loaded.
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

# xf 0: theme-black font; xf 1: blue font (input role). Counts all consistent
# so the only violation in fixtures below is the blue-font formula cell.
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

WORKBOOK_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL}">
  <sheets>
    <sheet name="Sheet1" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>
"""

RELS_TEMPLATE = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
    Target="{target}"/>
</Relationships>
"""

# One blue-font formula cell: a deliberate color-role violation the audit
# must find whenever the sheet is actually loaded.
WORKSHEET_TEMPLATE = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <sheetData>
    <row r="1"><c r="A1" s="1"><f>B2+1</f><v>101</v></c></row>
  </sheetData>
</worksheet>
"""


def build_xlsx(path: str, rel_target: str, sheet_member: str) -> str:
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("[Content_Types].xml",
                   '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
                   '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
                   '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
                   '<Default Extension="xml" ContentType="application/xml"/>'
                   '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
                   '</Types>')
        z.writestr("xl/workbook.xml", WORKBOOK_XML)
        z.writestr("xl/_rels/workbook.xml.rels", RELS_TEMPLATE.format(target=rel_target))
        z.writestr("xl/styles.xml", STYLES_XML)
        z.writestr(sheet_member, WORKSHEET_TEMPLATE)
    return path


def run_audit(path: str) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, SCRIPT, path],
                          capture_output=True, text=True)


class TestStyleAuditSheetResolution(unittest.TestCase):
    def test_package_absolute_target_sheets_are_audited(self):
        """Target="/xl/worksheets/sheet1.xml" must load and audit the sheet (issue #3649)."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "abs.xlsx"),
                              "/xl/worksheets/sheet1.xml",
                              "xl/worksheets/sheet1.xml")
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 1, result.stdout)
            self.assertIn("1 inspected", result.stdout)
            self.assertNotIn("0 inspected", result.stdout)
            self.assertIn("formula cell has blue font", result.stdout)

    def test_relative_target_still_audited(self):
        """The OPC-default relative Target keeps working."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "rel.xlsx"),
                              "worksheets/sheet1.xml",
                              "xl/worksheets/sheet1.xml")
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 1, result.stdout)
            self.assertIn("1 inspected", result.stdout)

    def test_zero_sheets_fail_loudly(self):
        """Unresolvable sheet targets must exit 1, never a 0-cell PASS."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "empty.xlsx"),
                              "/xl/worksheets/sheet9.xml",
                              "xl/worksheets/sheet1.xml")
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 1, result.stdout)
            self.assertIn("no worksheets", result.stdout)
            self.assertNotIn("PASS", result.stdout)

    def test_blue_formula_cell_counted_once(self):
        """A blue-font formula cell is one formula cell, not two.

        The C2 violation branch used to bump the formula_cells tally as well
        as the per-cell tally at the bottom of the loop, so the summary
        reported formula_cells=2 for a single-cell sheet.
        """
        import json
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "count.xlsx"),
                              "/xl/worksheets/sheet1.xml",
                              "xl/worksheets/sheet1.xml")
            result = subprocess.run([sys.executable, SCRIPT, xlsx, "--json"],
                                    capture_output=True, text=True)
            summary = json.loads(result.stdout)["summary"]
            self.assertEqual(summary["total_cells_inspected"], 1)
            self.assertEqual(summary["formula_cells"], 1, summary)
            self.assertEqual(summary["violations"], 1, summary)


if __name__ == "__main__":
    unittest.main()
