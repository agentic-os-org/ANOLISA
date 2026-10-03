#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""End-to-end tests for xlsx_insert_row.py on unpacked-xlsx work dirs.

Regression tests for worksheet rel Target resolution: the OPC default
(and templates/minimal_xlsx convention) is a Target relative to xl/,
e.g. Target="worksheets/sheet1.xml". Package-absolute targets
(Target="/xl/worksheets/sheet1.xml") must keep working.
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
SCRIPT = os.path.join(SCRIPTS_DIR, "xlsx_insert_row.py")


def _tag(local: str) -> str:
    return f"{{{NS_MAIN}}}{local}"


WORKBOOK_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL}">
  <sheets>
    <sheet name="Sheet1" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>
"""

WORKSHEET_TEMPLATE = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <dimension ref="A1:B2"/>
  <sheetData>
    <row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><v>10</v></c></row>
    <row r="2"><c r="A2" t="s"><v>1</v></c><c r="B2"><v>20</v></c></row>
  </sheetData>
</worksheet>
"""

SHARED_STRINGS = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="{NS_MAIN}" count="2" uniqueCount="2">
  <si><t>Alpha</t></si>
  <si><t>Beta</t></si>
</sst>
"""

RELS_TEMPLATE = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
    Target="{target}"/>
</Relationships>
"""


def build_work_dir(root: str, rel_target: str) -> str:
    """Build a minimal unpacked-xlsx work dir with the given rel Target."""
    work_dir = os.path.join(root, "work")
    os.makedirs(os.path.join(work_dir, "xl", "worksheets"))
    os.makedirs(os.path.join(work_dir, "xl", "_rels"))
    with open(os.path.join(work_dir, "xl", "workbook.xml"), "w") as f:
        f.write(WORKBOOK_XML)
    with open(os.path.join(work_dir, "xl", "_rels", "workbook.xml.rels"), "w") as f:
        f.write(RELS_TEMPLATE.format(target=rel_target))
    with open(os.path.join(work_dir, "xl", "worksheets", "sheet1.xml"), "w") as f:
        f.write(WORKSHEET_TEMPLATE)
    with open(os.path.join(work_dir, "xl", "sharedStrings.xml"), "w") as f:
        f.write(SHARED_STRINGS)
    return work_dir


def run_insert(work_dir: str, *extra: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, SCRIPT, work_dir, "--at", "2",
         "--text", "A=New", "--values", "B=55", *extra],
        capture_output=True, text=True,
    )


def load_sheet(work_dir: str) -> ET.Element:
    return ET.parse(os.path.join(work_dir, "xl", "worksheets", "sheet1.xml")).getroot()


def cell_map(root: ET.Element) -> dict:
    return {c.get("r"): c for c in root.iter(_tag("c"))}


class TestInsertRowRelTarget(unittest.TestCase):
    def test_insert_row_with_relative_rel_target(self):
        """OPC-default relative Target must resolve against xl/ (issue #3645)."""
        with tempfile.TemporaryDirectory() as root:
            work_dir = build_work_dir(root, "worksheets/sheet1.xml")
            result = run_insert(work_dir)
            self.assertEqual(result.returncode, 0, result.stderr)
            cells = cell_map(load_sheet(work_dir))
            # New row inserted at 2: A2 is shared string "New" (index 2), B2 = 55
            self.assertEqual(cells["A2"].get("t"), "s")
            self.assertEqual(cells["A2"].find(_tag("v")).text, "2")
            self.assertEqual(cells["B2"].find(_tag("v")).text, "55")
            # Old row 2 shifted down to 3 intact
            self.assertEqual(cells["B3"].find(_tag("v")).text, "20")

    def test_insert_row_with_absolute_rel_target(self):
        """Package-absolute Target must keep resolving against the root."""
        with tempfile.TemporaryDirectory() as root:
            work_dir = build_work_dir(root, "/xl/worksheets/sheet1.xml")
            result = run_insert(work_dir)
            self.assertEqual(result.returncode, 0, result.stderr)
            cells = cell_map(load_sheet(work_dir))
            self.assertEqual(cells["B2"].find(_tag("v")).text, "55")
            self.assertEqual(cells["B3"].find(_tag("v")).text, "20")

    def test_unknown_sheet_aborts_before_shift(self):
        """A bad sheet name must abort without leaving the dir half-shifted."""
        with tempfile.TemporaryDirectory() as root:
            work_dir = build_work_dir(root, "worksheets/sheet1.xml")
            result = run_insert(work_dir, "--sheet", "NoSuchSheet")
            self.assertEqual(result.returncode, 1)
            rows = [r.get("r") for r in load_sheet(work_dir).iter(_tag("row"))]
            self.assertEqual(rows, ["1", "2"])  # nothing shifted, nothing inserted


if __name__ == "__main__":
    unittest.main()
