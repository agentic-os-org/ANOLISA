#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Regression coverage for keeping table filter and sort ranges in step."""

import importlib.util
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path
from unittest import mock

SCRIPT = (
    Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts/xlsx_shift_rows.py"
)
SPEC = importlib.util.spec_from_file_location("xlsx_shift_rows", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
shift_rows = importlib.util.module_from_spec(SPEC)
with mock.patch.object(sys, "path", [str(SCRIPT.parent), *sys.path]):
    SPEC.loader.exec_module(shift_rows)
NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"


class TableRangeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.table_path = Path(self.temp_dir.name) / "table1.xml"
        self.table_path.write_text(
            f'<table xmlns="{NS}" id="1" name="Sales" displayName="Sales" '
            'ref="A1:D10" totalsRowCount="1">'
            '<autoFilter ref="A1:D9"><filterColumn colId="0">'
            '<filters><filter val="North"/></filters></filterColumn>'
            '<sortState ref="A2:D9"><sortCondition ref="B2:B9" descending="1"/>'
            "</sortState></autoFilter>"
            '<sortState ref="A2:D9"><sortCondition ref="C2:C9"/></sortState>'
            '<tableColumns count="1"><tableColumn id="1" name="Region"/>'
            "</tableColumns></table>",
            encoding="utf-8",
        )

    def assert_ranges(self, table: str, filter_range: str, sort_range: str) -> None:
        root = ET.parse(self.table_path).getroot()
        self.assertEqual(root.get("ref"), table)
        self.assertEqual(root.find(f"{{{NS}}}autoFilter").get("ref"), filter_range)
        for sort in root.iter(f"{{{NS}}}sortState"):
            self.assertEqual(sort.get("ref"), sort_range)
        end_row = sort_range.split(":")[1][1:]
        self.assertEqual(
            [condition.get("ref") for condition in root.iter(f"{{{NS}}}sortCondition")],
            [f"B2:B{end_row}", f"C2:C{end_row}"],
        )
        self.assertEqual(root.find(f".//{{{NS}}}filter").get("val"), "North")
        self.assertEqual(root.find(f".//{{{NS}}}sortCondition").get("descending"), "1")
        self.assertEqual(root.get("totalsRowCount"), "1")

    def test_insert_expands_filter_and_sort_ranges_without_including_totals(self) -> None:
        shift_rows.process_table(str(self.table_path), 5, 2)
        self.assert_ranges("A1:D12", "A1:D11", "A2:D11")

    def test_delete_contracts_filter_and_sort_ranges(self) -> None:
        shift_rows.process_table(str(self.table_path), 5, -2)
        self.assert_ranges("A1:D8", "A1:D7", "A2:D7")

    def test_table_above_edit_is_left_byte_identical(self) -> None:
        original = self.table_path.read_bytes()
        self.assertEqual(shift_rows.process_table(str(self.table_path), 11, 1), 0)
        self.assertEqual(self.table_path.read_bytes(), original)


if __name__ == "__main__":
    unittest.main()
