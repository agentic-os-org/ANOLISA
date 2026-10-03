#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for style_audit.py summary counts.

Regression test for the double-counted blue-font formula cells: a cell
must be classified as formula OR input exactly once, so the summary
counts partition the inspected cells.
"""

import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import style_audit  # noqa: E402

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "style_audit.py")

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

RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
    Target="worksheets/sheet1.xml"/>
</Relationships>
"""

# 3 cells: A1 blue-font formula (violation), B1 black-font formula, C1 blue input.
WORKSHEET_TEMPLATE = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <sheetData>
    <row r="1">
      <c r="A1" s="1"><f>B2+1</f><v>101</v></c>
      <c r="B1" s="0"><f>A1*2</f><v>202</v></c>
      <c r="C1" s="1"><v>5</v></c>
    </row>
  </sheetData>
</worksheet>
"""


def build_work_dir(root: str) -> str:
    work_dir = os.path.join(root, "work")
    os.makedirs(os.path.join(work_dir, "xl", "worksheets"))
    os.makedirs(os.path.join(work_dir, "xl", "_rels"))
    for rel_path, content in (
        ("xl/workbook.xml", WORKBOOK_XML),
        ("xl/_rels/workbook.xml.rels", RELS),
        ("xl/styles.xml", STYLES_XML),
        ("xl/worksheets/sheet1.xml", WORKSHEET_TEMPLATE),
    ):
        with open(os.path.join(work_dir, rel_path), "w") as f:
            f.write(content)
    return work_dir


class TestStyleAuditSummaryCounts(unittest.TestCase):
    def test_counts_partition_inspected_cells(self):
        """3-cell sheet: 2 formula + 1 input == 3 inspected (issue #3650)."""
        results = style_audit._audit(
            STYLES_XML.encode("utf-8"),
            [("Sheet1", WORKSHEET_TEMPLATE.encode("utf-8"))],
        )
        s = results["summary"]
        self.assertEqual(s["total_cells_inspected"], 3)
        self.assertEqual(s["formula_cells"], 2)
        self.assertEqual(s["input_cells"], 1)
        # The blue-font formula cell is still flagged exactly once
        blue = [v for v in results["violations"] if v["type"] == "formula_cell_blue_font"]
        self.assertEqual(len(blue), 1)
        self.assertEqual(blue[0]["cell"], "A1")

    def test_cli_summary_line_sums_to_total(self):
        """End-to-end: the printed '(N formula, M input)' sums to the total."""
        with tempfile.TemporaryDirectory() as root:
            work_dir = build_work_dir(root)
            result = subprocess.run(
                [sys.executable, SCRIPT, work_dir, "--summary"],
                capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 1, result.stdout)  # 1 violation
            self.assertIn("3 inspected  (2 formula, 1 input)", result.stdout)


if __name__ == "__main__":
    unittest.main()
