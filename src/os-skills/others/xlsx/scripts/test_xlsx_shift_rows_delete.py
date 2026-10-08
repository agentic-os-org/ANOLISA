#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_shift_rows.py row deletion.

Deleting rows must remove the <row> elements inside the deleted range.
The renumber-only implementation used to keep them, shifted up into row
numbers that still existed above the deletion point, so sheetData ended
up with duplicate r= attributes (and duplicate cell references) — an
xlsx Excel insists on repairing.
"""

import os
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
if SCRIPTS_DIR not in sys.path:
    sys.path.insert(0, SCRIPTS_DIR)

from xlsx_shift_rows import process_worksheet  # noqa: E402

NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"


def _worksheet_xml(last_row: int = 6) -> str:
    rows = "".join(
        f'<row r="{r}"><c r="A{r}"><v>{r}</v></c></row>' for r in range(1, last_row + 1)
    )
    return (
        '<?xml version="1.0"?>'
        f'<worksheet xmlns="{NS}"><sheetData>{rows}</sheetData></worksheet>'
    )


def _write_worksheet(tmpdir: str, xml: str) -> str:
    path = os.path.join(tmpdir, "sheet1.xml")
    with open(path, "w", encoding="utf-8") as f:
        f.write(xml)
    return path


def _row_numbers(path: str) -> list[int]:
    root = ET.parse(path).getroot()
    return [int(r.get("r")) for r in root.find(f"{{{NS}}}sheetData")]


def _cell_values(path: str) -> dict[int, str]:
    root = ET.parse(path).getroot()
    out = {}
    for row in root.find(f"{{{NS}}}sheetData"):
        v = row.find(f"{{{NS}}}c/{{{NS}}}v")
        out[int(row.get("r"))] = v.text
    return out


class TestDeleteRows(unittest.TestCase):
    def test_delete_removes_rows_inside_the_range(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            path = _write_worksheet(tmpdir, _worksheet_xml())
            process_worksheet(path, at=3, delta=-2)  # delete 2 rows at row 3
            self.assertEqual(_row_numbers(path), [1, 2, 3, 4])
            # Rows 5 and 6 shifted up to 3 and 4, keeping their values.
            self.assertEqual(_cell_values(path), {1: "1", 2: "2", 3: "5", 4: "6"})

    def test_delete_keeps_row_numbers_unique(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            path = _write_worksheet(tmpdir, _worksheet_xml())
            process_worksheet(path, at=3, delta=-2)
            nums = _row_numbers(path)
            self.assertEqual(len(nums), len(set(nums)))

    def test_delete_last_rows(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            path = _write_worksheet(tmpdir, _worksheet_xml())
            process_worksheet(path, at=5, delta=-2)  # delete rows 5 and 6
            self.assertEqual(_row_numbers(path), [1, 2, 3, 4])

    def test_insert_still_renumbers_only(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            path = _write_worksheet(tmpdir, _worksheet_xml())
            process_worksheet(path, at=3, delta=2)  # insert 2 rows at row 3
            self.assertEqual(_row_numbers(path), [1, 2, 5, 6, 7, 8])
            self.assertEqual(_cell_values(path),
                             {1: "1", 2: "2", 5: "3", 6: "4", 7: "5", 8: "6"})


if __name__ == "__main__":
    unittest.main()
