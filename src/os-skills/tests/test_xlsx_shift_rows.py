#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_shift_rows.py shared-formula handling.

A shared formula group is declared on its master cell as
``<f t="shared" ref="C2:C4" si="0">``, and every consumer that shares ``si``
is only valid inside that range. Shifting rows moved the consumers but left
``ref`` behind, so a relocated consumer ended up outside the declared range,
which is invalid OOXML and makes Excel offer to repair the file.
"""

import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

sys.dont_write_bytecode = True

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
SCRIPTS_DIR = Path(__file__).resolve().parents[1] / "others" / "xlsx" / "scripts"
SCRIPT = SCRIPTS_DIR / "xlsx_shift_rows.py"

SHEET_WITH_SHARED_FORMULA = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <dimension ref="A1:C4"/>
  <sheetData>
    <row r="2"><c r="B2"><v>1</v></c><c r="C2"><f t="shared" ref="C2:C4" si="0">B2*2</f><v>2</v></c></row>
    <row r="3"><c r="C3"><f t="shared" si="0"/><v>4</v></c></row>
    <row r="4"><c r="C4"><f t="shared" si="0"/><v>6</v></c></row>
  </sheetData>
</worksheet>
"""


class TestShiftSharedFormulaRef(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.worksheet = Path(self.tmpdir) / "xl" / "worksheets" / "sheet1.xml"
        self.worksheet.parent.mkdir(parents=True)
        self.worksheet.write_text(SHEET_WITH_SHARED_FORMULA, encoding="utf-8")

    def _run(self, operation="insert", at="3", count="1"):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), self.tmpdir, operation, at, count],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return ET.parse(self.worksheet).getroot()

    def _shared_master(self, root):
        for formula in root.iter(f"{{{NS_MAIN}}}f"):
            if formula.get("t") == "shared" and formula.get("ref"):
                return formula
        self.fail("shared formula master without a ref attribute")

    def test_insert_shifts_the_shared_group_range(self):
        root = self._run("insert", "3", "1")

        master = self._shared_master(root)
        self.assertEqual(master.get("ref"), "C2:C5")

    def test_consumer_cells_move_inside_the_shifted_range(self):
        root = self._run("insert", "3", "1")

        refs = sorted(cell.get("r") for cell in root.iter(f"{{{NS_MAIN}}}c"))
        self.assertEqual(refs, ["B2", "C2", "C4", "C5"])

    def test_shared_consumers_never_gain_a_ref(self):
        root = self._run("insert", "3", "1")

        for formula in root.iter(f"{{{NS_MAIN}}}f"):
            if formula.get("t") == "shared" and not (formula.text or "").strip():
                self.assertIsNone(formula.get("ref"))

    def test_formula_text_still_shifts_with_the_group(self):
        root = self._run("insert", "3", "1")

        master = self._shared_master(root)
        self.assertEqual(master.text, "B2*2")


if __name__ == "__main__":
    unittest.main()
