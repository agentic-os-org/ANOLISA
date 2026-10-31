#!/usr/bin/env python3
"""Regression tests for worksheet filter/sort ranges in xlsx_shift_rows.py (#4991).

``process_worksheet`` shifted the dimension, rows, cells, formulas, merged
ranges, conditional formatting and data validations, but not the worksheet's
own ``<autoFilter ref>``, ``<sortState ref>`` or ``<sortCondition ref>``.  After
an insert the sheet's filter and sort ranges kept addressing the pre-edit rows,
so the newly inserted rows were silently excluded from filtering and sorting;
after a delete the ranges kept addressing rows that no longer exist.

Ranges owned by ``xl/tables/*.xml`` are a separate part handled by
``process_table``; these cases cover ``xl/worksheets/*.xml`` only.

Drives the real CLI (unpack -> shift -> inspect) over a two-sheet fixture built
with zipfile: sheet1 holds 11 rows plus ``autoFilter A1:D11`` nesting
``sortState A2:D11`` and ``sortCondition B2:B11``; sheet2 holds 3 rows plus
``autoFilter A1:D3`` so that a range entirely above the edit point is provably
left alone.

Baseline on unchanged main: the insert and delete cases fail with the stale
``A1:D11`` / ``A2:D11`` / ``B2:B11`` ranges.
"""

import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path

SCRIPTS = (
    Path(__file__).resolve().parents[2]
    / "src"
    / "os-skills"
    / "others"
    / "xlsx"
    / "scripts"
)
UNPACK = SCRIPTS / "xlsx_unpack.py"
SHIFT = SCRIPTS / "xlsx_shift_rows.py"

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
NS_PKG_REL = "http://schemas.openxmlformats.org/package/2006/relationships"

CONTENT_TYPES = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
  <Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>
"""

ROOT_RELS = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="{NS_PKG_REL}">
  <Relationship Id="rId1"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
    Target="xl/workbook.xml"/>
</Relationships>
"""

WORKBOOK_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL}">
  <sheets>
    <sheet name="Data" sheetId="1" r:id="rId1"/>
    <sheet name="Above" sheetId="2" r:id="rId2"/>
  </sheets>
</workbook>
"""

WORKBOOK_RELS = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="{NS_PKG_REL}">
  <Relationship Id="rId1"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
    Target="worksheets/sheet1.xml"/>
  <Relationship Id="rId2"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
    Target="worksheets/sheet2.xml"/>
</Relationships>
"""


def _rows(count: int, cols: int = 4) -> str:
    letters = ["A", "B", "C", "D"][:cols]
    out = []
    for r in range(1, count + 1):
        cells = "".join(f'<c r="{c}{r}"><v>{r}</v></c>' for c in letters)
        out.append(f'<row r="{r}">{cells}</row>')
    return "".join(out)


# 11 data rows with a sheet-level filter whose sort state and sort condition
# both span the edit point (row 5).
SHEET1_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <dimension ref="A1:D11"/>
  <sheetData>{_rows(11)}</sheetData>
  <autoFilter ref="A1:D11">
    <sortState ref="A2:D11">
      <sortCondition ref="B2:B11"/>
    </sortState>
  </autoFilter>
</worksheet>
"""

# 3 rows only: every range sits above the edit point and must not move.
SHEET2_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <dimension ref="A1:D3"/>
  <sheetData>{_rows(3)}</sheetData>
  <autoFilter ref="A1:D3"/>
</worksheet>
"""


def build_xlsx(path: Path) -> None:
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", CONTENT_TYPES)
        z.writestr("_rels/.rels", ROOT_RELS)
        z.writestr("xl/workbook.xml", WORKBOOK_XML)
        z.writestr("xl/_rels/workbook.xml.rels", WORKBOOK_RELS)
        z.writestr("xl/worksheets/sheet1.xml", SHEET1_XML)
        z.writestr("xl/worksheets/sheet2.xml", SHEET2_XML)


def _tag(local: str) -> str:
    return f"{{{NS_MAIN}}}{local}"


class XlsxShiftSheetFilterRangesTestCase(unittest.TestCase):
    """Unpack the fixture, run the real shift CLI, inspect both worksheets."""

    EDIT_ROW = "5"
    COUNT = "2"

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        xlsx = self.root / "in.xlsx"
        build_xlsx(xlsx)
        self.work = self.root / "work"
        self.run_script(UNPACK, str(xlsx), str(self.work))
        self.sheet1 = self.work / "xl" / "worksheets" / "sheet1.xml"
        self.sheet2 = self.work / "xl" / "worksheets" / "sheet2.xml"

    def run_script(self, script: Path, *args: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(script), *args],
            capture_output=True,
            text=True,
            timeout=120,
        )

    def shift(self, operation: str) -> subprocess.CompletedProcess:
        return self.run_script(
            SHIFT, str(self.work), operation, self.EDIT_ROW, self.COUNT
        )

    def refs(self, path: Path) -> dict:
        root = ET.parse(path).getroot()
        found = {}
        for local in ("dimension", "autoFilter", "sortState", "sortCondition"):
            el = root.find(f".//{_tag(local)}")
            found[local] = None if el is None else el.get("ref")
        return found

    def assertShiftSucceeded(self, proc: subprocess.CompletedProcess) -> None:
        self.assertEqual(
            proc.returncode,
            0,
            f"xlsx_shift_rows.py failed\nstdout: {proc.stdout}\nstderr: {proc.stderr}",
        )

    def test_insert_expands_sheet_filter_and_sort_ranges(self):
        self.assertShiftSucceeded(self.shift("insert"))
        self.assertEqual(
            self.refs(self.sheet1),
            {
                "dimension": "A1:D13",
                "autoFilter": "A1:D13",
                "sortState": "A2:D13",
                "sortCondition": "B2:B13",
            },
        )

    def test_delete_contracts_sheet_filter_and_sort_ranges(self):
        self.assertShiftSucceeded(self.shift("delete"))
        self.assertEqual(
            self.refs(self.sheet1),
            {
                "dimension": "A1:D9",
                "autoFilter": "A1:D9",
                "sortState": "A2:D9",
                "sortCondition": "B2:B9",
            },
        )

    def test_ranges_above_the_edit_point_are_untouched(self):
        self.assertShiftSucceeded(self.shift("insert"))
        self.assertEqual(self.refs(self.sheet2)["autoFilter"], "A1:D3")
        self.assertEqual(self.refs(self.sheet2)["dimension"], "A1:D3")

    def test_shifted_rows_stay_inside_the_filter_range(self):
        """Rows >= the edit point move down; the range must cover them again.

        xlsx_shift_rows only renumbers existing rows (the caller fills the
        vacated slots), so after `insert 5 2` the sheet holds rows 1-4 and
        7-13 while the filter/sort ranges have to reach row 13.
        """
        self.assertShiftSucceeded(self.shift("insert"))
        data = ET.parse(self.sheet1).getroot().find(_tag("sheetData"))
        rows = [int(row.get("r")) for row in data.findall(_tag("row"))]
        self.assertEqual(rows, [1, 2, 3, 4, 7, 8, 9, 10, 11, 12, 13])
        self.assertEqual(self.refs(self.sheet1)["autoFilter"], f"A1:D{rows[-1]}")
        self.assertEqual(self.refs(self.sheet1)["sortState"], f"A2:D{rows[-1]}")


if __name__ == "__main__":
    unittest.main()
