#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for empty/non-numeric `s` (style) attributes in the xlsx edit scripts.

Regression tests for cells whose `s` attribute is empty (`s=""`) or
non-numeric (`s="abc"`), as written by some third-party tools and by
hand-edited XML. Both editors parsed the attribute with
`int(cell.get("s", "0"))` and aborted with a ValueError traceback:
xlsx_insert_row.py crashed *after* xlsx_shift_rows.py had already rewritten
the workbook, leaving a half-applied edit that a retry would shift twice,
and xlsx_add_column.py crashed before adding anything. style_audit.py
already tolerates such values; the editors must fall back to the default
style index 0 instead.
"""

import os
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
INSERT_ROW = os.path.join(SCRIPTS_DIR, "xlsx_insert_row.py")
ADD_COLUMN = os.path.join(SCRIPTS_DIR, "xlsx_add_column.py")

STYLES_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="{NS_MAIN}">
  <fonts count="1"><font><sz val="11"/><color theme="1"/></font></fonts>
  <fills count="2">
    <fill><patternFill patternType="none"/></fill>
    <fill><patternFill patternType="gray125"/></fill>
  </fills>
  <borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders>
  <cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
  <cellXfs count="3">
    <xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>
    <xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0" applyFont="1"/>
    <xf numFmtId="2" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
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

# Package-absolute rel target, the form both editors resolve (see also
# test_style_audit_sheets.py).
RELS_XML = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
    Target="/xl/worksheets/sheet1.xml"/>
</Relationships>
"""

SST_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="{NS_MAIN}" count="3" uniqueCount="3">
  <si><t>Item</t></si><si><t>Widget</t></si><si><t>Gadget</t></si>
</sst>
"""

# Row 1 is well formed and must keep being read as before; row 2 carries an
# empty and row 3 a non-numeric `s` in both columns.
SHEET_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <dimension ref="A1:B3"/>
  <sheetData>
    <row r="1"><c r="A1" t="s" s="1"><v>0</v></c><c r="B1" s="1"><v>1</v></c></row>
    <row r="2"><c r="A2" t="s" s=""><v>1</v></c><c r="B2" s=""><v>10</v></c></row>
    <row r="3"><c r="A3" t="s" s="abc"><v>2</v></c><c r="B3" s="2"><v>20</v></c></row>
  </sheetData>
</worksheet>
"""


def build_work_dir(root: str, sheet_xml: str = SHEET_XML) -> str:
    """Lay out a minimal unpacked xlsx work dir and return its path."""
    for rel in ("xl/_rels", "xl/worksheets"):
        os.makedirs(os.path.join(root, rel), exist_ok=True)
    parts = {
        "xl/workbook.xml": WORKBOOK_XML,
        "xl/_rels/workbook.xml.rels": RELS_XML,
        "xl/styles.xml": STYLES_XML,
        "xl/sharedStrings.xml": SST_XML,
        "xl/worksheets/sheet1.xml": sheet_xml,
    }
    for rel, content in parts.items():
        with open(os.path.join(root, rel), "w", encoding="utf-8") as fh:
            fh.write(content)
    return root


def run_script(script: str, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, script, *args],
                          capture_output=True, text=True)


def cells_by_ref(work_dir: str) -> dict:
    """Return {cell_ref: element} for xl/worksheets/sheet1.xml."""
    path = os.path.join(work_dir, "xl", "worksheets", "sheet1.xml")
    root = ET.parse(path).getroot()
    return {c.get("r"): c for c in root.iter(f"{{{NS_MAIN}}}c")}


def rows_by_number(work_dir: str) -> dict:
    path = os.path.join(work_dir, "xl", "worksheets", "sheet1.xml")
    root = ET.parse(path).getroot()
    return {r.get("r"): r for r in root.iter(f"{{{NS_MAIN}}}row")}


class TestInsertRowInvalidStyle(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.work_dir = build_work_dir(self.tmp.name)

    def test_invalid_style_in_reference_row_is_read_as_default(self):
        """s="" on the copied row must not abort the insert (issue #5016)."""
        result = run_script(INSERT_ROW, self.work_dir, "--at", "3",
                            "--text", "A=Utilities", "--values", "B=30",
                            "--copy-style-from", "2")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("Traceback", result.stderr)

        cells = cells_by_ref(self.work_dir)
        # The new row exists and both invalid styles fell back to index 0.
        self.assertEqual(cells["A3"].get("s"), "0")
        self.assertEqual(cells["B3"].get("s"), "0")
        self.assertEqual(cells["B3"].find(f"{{{NS_MAIN}}}v").text, "30")

    def test_valid_style_in_reference_row_is_still_copied(self):
        """The well-formed reference row keeps copying its real style."""
        result = run_script(INSERT_ROW, self.work_dir, "--at", "3",
                            "--text", "A=Utilities", "--values", "B=30",
                            "--copy-style-from", "1")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

        cells = cells_by_ref(self.work_dir)
        self.assertEqual(cells["A3"].get("s"), "1")
        self.assertEqual(cells["B3"].get("s"), "1")

    def test_shift_is_applied_exactly_once(self):
        """A crash after the shift left a half-edited dir; the shift must run once."""
        result = run_script(INSERT_ROW, self.work_dir, "--at", "3",
                            "--text", "A=Utilities", "--values", "B=30",
                            "--copy-style-from", "2")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

        rows = rows_by_number(self.work_dir)
        self.assertIn("4", rows)
        self.assertNotIn("5", rows)
        # The original row 3 (non-numeric s="abc") moved down to row 4 intact.
        cells = cells_by_ref(self.work_dir)
        self.assertEqual(cells["A4"].get("s"), "abc")
        self.assertEqual(cells["A4"].find(f"{{{NS_MAIN}}}v").text, "2")


class TestAddColumnInvalidStyle(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.work_dir = build_work_dir(self.tmp.name)

    def test_invalid_style_in_previous_column_is_read_as_default(self):
        """s="" on the previous column must not abort the add (issue #5016)."""
        result = run_script(ADD_COLUMN, self.work_dir, "--col", "C",
                            "--header", "Extra", "--formula", "=B{row}*2",
                            "--formula-rows", "2:3")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("Traceback", result.stderr)

        cells = cells_by_ref(self.work_dir)
        # Header style comes from B1 (valid), data style from B2 (s="" → 0).
        self.assertEqual(cells["C1"].get("s"), "1")
        self.assertEqual(cells["C2"].get("s"), "0")
        self.assertEqual(cells["C3"].get("s"), "0")
        self.assertEqual(cells["C2"].find(f"{{{NS_MAIN}}}f").text, "B2*2")
        self.assertEqual(cells["C3"].find(f"{{{NS_MAIN}}}f").text, "B3*2")

    def test_border_row_with_invalid_style_clones_the_default(self):
        """--border-row must survive a non-numeric s on the row it rewrites."""
        result = run_script(ADD_COLUMN, self.work_dir, "--col", "C",
                            "--border-row", "3")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("Traceback", result.stderr)

        cells = cells_by_ref(self.work_dir)
        # Both cells of row 3 were remapped onto the cloned bordered styles.
        for ref in ("A3", "B3"):
            self.assertTrue(cells[ref].get("s").isdigit(), cells[ref].get("s"))
            self.assertNotEqual(cells[ref].get("s"), "abc")

        styles = ET.parse(os.path.join(self.work_dir, "xl", "styles.xml")).getroot()
        borders = styles.find(f"{{{NS_MAIN}}}borders")
        cellxfs = styles.find(f"{{{NS_MAIN}}}cellXfs")
        self.assertEqual(borders.get("count"), "2")
        # One clone per distinct source style in the row: s="abc" → 0 and s="2".
        self.assertEqual(len(list(cellxfs)), 5)
        self.assertEqual(cellxfs.get("count"), "5")


if __name__ == "__main__":
    unittest.main()
