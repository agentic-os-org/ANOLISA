#!/usr/bin/env python3
"""Regression tests for xlsx_insert_row.py row inserts (#3743).

Drives the real CLI over a synthetic unpacked workbook. A style-only
insert (``--copy-style-from`` with no ``--text/--values/--formula``) builds a
row holding zero cells, and the dimension update fed that empty column list
straight into ``max()``.

Baseline on unchanged main: 1 of the 3 tests fails — the blank insert dies
with ``ValueError: max() arg is an empty sequence`` *after* Step 1 already
rewrote the sheet on disk, so the work dir comes back half-mutated (rows
``1,2,3,4,5,7,...,11``, no row 6) and a retry shifts every following row a
second time. The two value-insert tests pass on main as well: they pin the
widened and the unchanged dimension branches of the refactored
widest-column lookup as equivalent to the old behaviour.
"""

import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src"
    / "os-skills"
    / "others"
    / "xlsx"
    / "scripts"
    / "xlsx_insert_row.py"
)

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
NS_PKG_REL = "http://schemas.openxmlformats.org/package/2006/relationships"

# The rel target is package-absolute because the script resolves targets
# against the work dir root, not against xl/ — the relative form Excel and
# the skill's own minimal_xlsx template write is tracked separately (#3645).
SHEET_TARGET = "/xl/worksheets/sheet1.xml"


def _local(tag):
    return tag.rpartition("}")[2]


def build_work_dir(root, rows=10, columns="ABCD", dimension="A1:D10"):
    """Write an unpacked single-sheet workbook with ``rows`` styled rows."""
    (root / "xl" / "_rels").mkdir(parents=True, exist_ok=True)
    (root / "xl" / "worksheets").mkdir(parents=True, exist_ok=True)

    (root / "xl" / "workbook.xml").write_text(
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        f'<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL}"><sheets>'
        '<sheet name="Budget" sheetId="1" r:id="rId1"/></sheets></workbook>\n',
        encoding="utf-8",
    )
    (root / "xl" / "_rels" / "workbook.xml.rels").write_text(
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        f'<Relationships xmlns="{NS_PKG_REL}">'
        f'<Relationship Id="rId1" Type="{NS_REL}/worksheet" Target="{SHEET_TARGET}"/>'
        "</Relationships>\n",
        encoding="utf-8",
    )
    (root / "xl" / "sharedStrings.xml").write_text(
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        f'<sst xmlns="{NS_MAIN}" count="1" uniqueCount="1">'
        "<si><t>Label</t></si></sst>\n",
        encoding="utf-8",
    )

    body = []
    for r in range(1, rows + 1):
        # Row 5 is the --copy-style-from source: style 1 vs 2 elsewhere.
        style = "1" if r == 5 else "2"
        cells = "".join(f'<c r="{c}{r}" s="{style}"><v>{r}</v></c>' for c in columns)
        body.append(f'<row r="{r}">{cells}</row>')
    (root / "xl" / "worksheets" / "sheet1.xml").write_text(
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        f'<worksheet xmlns="{NS_MAIN}"><dimension ref="{dimension}"/>'
        f'<sheetData>{"".join(body)}</sheetData></worksheet>\n',
        encoding="utf-8",
    )
    return root


@unittest.skipIf(
    sys.version_info < (3, 10),
    "xlsx_insert_row.py uses PEP 604 annotations (Python 3.10+)",
)
class InsertRowTestCase(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.work = build_work_dir(Path(tmp.name) / "work")
        self.sheet = self.work / "xl" / "worksheets" / "sheet1.xml"

    def run_cli(self, *args):
        return subprocess.run(
            [sys.executable, str(SCRIPT), str(self.work), *args],
            capture_output=True,
            text=True,
        )

    def sheet_root(self):
        return ET.parse(self.sheet).getroot()

    def row_numbers(self):
        sheet_data = next(e for e in self.sheet_root().iter() if _local(e.tag) == "sheetData")
        return [int(row.get("r")) for row in sheet_data if _local(row.tag) == "row"]

    def row_cells(self, row_number):
        sheet_data = next(e for e in self.sheet_root().iter() if _local(e.tag) == "sheetData")
        for row in sheet_data:
            if _local(row.tag) == "row" and row.get("r") == str(row_number):
                return {c.get("r"): c for c in row if _local(c.tag) == "c"}
        self.fail(f"row {row_number} missing from sheetData")

    def dimension(self):
        for el in self.sheet_root().iter():
            if _local(el.tag) == "dimension":
                return el.get("ref")
        return None


class TestBlankInsert(InsertRowTestCase):
    def test_style_only_insert_completes(self):
        """A style-only insert must exit 0 and leave rows 1..11 ascending (#3743)."""
        proc = self.run_cli("--at", "6", "--copy-style-from", "5")
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertNotIn("Traceback", proc.stderr)

        # The crash used to land after xlsx_shift_rows.py had already rewritten
        # the sheet, so also assert the on-disk result is complete: the new row
        # slot is there (empty — no --text/--values/--formula was given) and no
        # row was lost to the shift.
        self.assertEqual(self.row_numbers(), list(range(1, 12)))
        self.assertEqual(self.row_cells(6), {})
        self.assertEqual(self.dimension(), "A1:D11")


class TestDimensionWidening(InsertRowTestCase):
    def test_cell_past_end_column_widens_dimension(self):
        """A value insert reaching column F still widens A1:D10 to A1:F11."""
        proc = self.run_cli(
            "--at", "6",
            "--text", "A=Utilities",
            "--values", "B=3000",
            "--formula", "F=SUM(B{row}:E{row})",
            "--copy-style-from", "5",
        )
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertEqual(self.row_numbers(), list(range(1, 12)))
        self.assertEqual(self.dimension(), "A1:F11")

        cells = self.row_cells(6)
        self.assertEqual(sorted(cells), ["A6", "B6", "F6"])
        self.assertEqual(cells["A6"].get("t"), "s")
        self.assertEqual(cells["A6"].get("s"), "1")  # style copied from row 5
        self.assertEqual(cells["B6"].find(f"{{{NS_MAIN}}}v").text, "3000")
        self.assertEqual(
            cells["F6"].find(f"{{{NS_MAIN}}}f").text, "SUM(B6:E6)")

    def test_cell_inside_dimension_keeps_it(self):
        """A value insert inside the end column must not touch the columns."""
        proc = self.run_cli("--at", "6", "--values", "B=3000", "C=3100")
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertEqual(self.row_numbers(), list(range(1, 12)))
        self.assertEqual(self.dimension(), "A1:D11")

        cells = self.row_cells(6)
        self.assertEqual(sorted(cells), ["B6", "C6"])
        self.assertEqual(cells["C6"].find(f"{{{NS_MAIN}}}v").text, "3100")


if __name__ == "__main__":
    unittest.main()
