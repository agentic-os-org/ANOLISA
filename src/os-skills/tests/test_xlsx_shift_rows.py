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


# The input cells sit outside the table, which ECMA-376 §18.3.1.38 allows and
# real workbooks do: `r1` is above the insertion point and must not move, `r2`
# is at it and must. A fixture with both inputs inside `ref` would pin a
# containment rule that does not exist.
SHEET_WITH_DATA_TABLE = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <dimension ref="A1:D13"/>
  <sheetData>
    <row r="10"><c r="A10"><f t="dataTable" ref="A10:D13" r1="$B$2" r2="$A$5" ca="1"/><v>1</v></c></row>
    <row r="13"><c r="A13"><f t="dataTable" r1="$B$2" r2="$A$5"/><v>2</v></c></row>
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


class TestShiftDataTableRef(unittest.TestCase):
    """A `dataTable` formula declares its table three times over.

    `ref` is a range, `r1`/`r2` are the input cells, and all three describe the
    same table: shifting only `ref` leaves `r1`/`r2` pointing outside the range
    it just declared, which is the same invalid OOXML the shared-formula case
    was fixed for.
    """

    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.worksheet = Path(self.tmpdir) / "xl" / "worksheets" / "sheet1.xml"
        self.worksheet.parent.mkdir(parents=True)
        self.worksheet.write_text(SHEET_WITH_DATA_TABLE, encoding="utf-8")

    def _run(self, operation="insert", at="5", count="1"):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), self.tmpdir, operation, at, count],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return ET.parse(self.worksheet).getroot()

    def _data_tables(self, root):
        return [f for f in root.iter(f"{{{NS_MAIN}}}f") if f.get("t") == "dataTable"]

    def test_every_data_table_declaration_moves_together(self):
        root = self._run("insert", "5", "1")

        formulas = self._data_tables(root)
        self.assertEqual(len(formulas), 2, formulas)
        master = formulas[0]
        self.assertEqual(master.get("ref"), "A11:D14")
        self.assertEqual(master.get("r1"), "$B$2")
        self.assertEqual(master.get("r2"), "$A$6")

    def test_a_declaration_without_a_ref_also_follows_its_input_cells(self):
        root = self._run("insert", "5", "1")

        # The non-master declaration carries no `ref`; its `r1`/`r2` still have
        # to track the cells they point at, or the table reads the wrong input.
        non_master = self._data_tables(root)[1]
        self.assertIsNone(non_master.get("ref"))
        self.assertEqual(non_master.get("r1"), "$B$2")
        self.assertEqual(non_master.get("r2"), "$A$6")

    def test_an_input_cell_above_the_insertion_point_does_not_move(self):
        root = self._run("insert", "5", "1")

        # `$B$2` is row 2, above the inserted row, so the cell it points at has
        # not moved and neither should the declaration. Shifting it would make
        # the table read a different cell than the one the author chose.
        master = self._data_tables(root)[0]
        self.assertEqual(master.get("r1"), "$B$2")


if __name__ == "__main__":
    unittest.main()
