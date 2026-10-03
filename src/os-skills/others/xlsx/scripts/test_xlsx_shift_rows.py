#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_shift_rows.py on unpacked-xlsx work dirs.

Regression tests for shared-formula handling: the group range lives in
the ref attribute of the master <f t="shared" ref="..." si="...">, and
consumers are only valid inside that range — so the attribute must
shift in lockstep with the relocated group.
"""

import os
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "xlsx_shift_rows.py")


def _tag(local: str) -> str:
    return f"{{{NS_MAIN}}}{local}"


# Shared formula group in C2:C4 (master B2*2, consumers si="0"), total row 5.
WORKSHEET_TEMPLATE = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <dimension ref="A1:C5"/>
  <sheetData>
    <row r="1"><c r="B1"><v>10</v></c></row>
    <row r="2"><c r="B2"><v>100</v></c><c r="C2"><f t="shared" ref="C2:C4" si="0">B2*2</f><v>200</v></c></row>
    <row r="3"><c r="B3"><v>110</v></c><c r="C3"><f t="shared" si="0"/><v>220</v></c></row>
    <row r="4"><c r="B4"><v>120</v></c><c r="C4"><f t="shared" si="0"/><v>240</v></c></row>
    <row r="5"><c r="C5"><f>SUM(C2:C4)</f><v>660</v></c></row>
  </sheetData>
</worksheet>
"""


def build_work_dir(root: str) -> str:
    work_dir = os.path.join(root, "work")
    os.makedirs(os.path.join(work_dir, "xl", "worksheets"))
    with open(os.path.join(work_dir, "xl", "worksheets", "sheet1.xml"), "w") as f:
        f.write(WORKSHEET_TEMPLATE)
    return work_dir


def run_shift(work_dir: str, operation: str, at: int, count: int) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, SCRIPT, work_dir, operation, str(at), str(count)],
        capture_output=True, text=True,
    )


def load(work_dir: str) -> ET.Element:
    return ET.parse(os.path.join(work_dir, "xl", "worksheets", "sheet1.xml")).getroot()


def cell_map(root: ET.Element) -> dict:
    return {c.get("r"): c for c in root.iter(_tag("c"))}


def shared_master(cells: dict) -> ET.Element:
    for c in cells.values():
        f_el = c.find(_tag("f"))
        if f_el is not None and f_el.get("t") == "shared" and f_el.get("ref"):
            return f_el
    raise AssertionError("shared formula master not found")


class TestSharedFormulaRefShift(unittest.TestCase):
    def test_insert_inside_group_shifts_ref_end(self):
        """Insert 3 1: consumers move to C4/C5, ref must follow (C2:C4 → C2:C5)."""
        with tempfile.TemporaryDirectory() as root:
            work_dir = build_work_dir(root)
            result = run_shift(work_dir, "insert", 3, 1)
            self.assertEqual(result.returncode, 0, result.stderr)
            cells = cell_map(load(work_dir))
            f_el = shared_master(cells)
            self.assertEqual(f_el.get("ref"), "C2:C5")
            self.assertEqual(f_el.text, "B2*2")  # master stays at C2, refs < 3
            # Consumers relocated inside the declared range
            self.assertIn("C4", cells)
            self.assertIn("C5", cells)
            self.assertIsNotNone(cells["C5"].find(_tag("f")))
            # Total formula moved and expanded to cover the shifted group
            self.assertEqual(cells["C6"].find(_tag("f")).text, "SUM(C2:C5)")

    def test_insert_above_group_shifts_whole_ref(self):
        """Insert 1 1: whole group moves down, ref shifts with it (C2:C4 → C3:C5)."""
        with tempfile.TemporaryDirectory() as root:
            work_dir = build_work_dir(root)
            result = run_shift(work_dir, "insert", 1, 1)
            self.assertEqual(result.returncode, 0, result.stderr)
            cells = cell_map(load(work_dir))
            f_el = shared_master(cells)
            self.assertEqual(f_el.get("ref"), "C3:C5")
            self.assertEqual(f_el.text, "B3*2")
            self.assertIn("C3", cells)
            self.assertIn("C5", cells)


if __name__ == "__main__":
    unittest.main()
