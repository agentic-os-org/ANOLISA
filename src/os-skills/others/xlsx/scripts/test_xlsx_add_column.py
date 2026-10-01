#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Rows created for formula/total cells must keep sheetData ascending.

Both row-creation sites appended new <row> elements at the end of sheetData
regardless of their row number, so a sheet with rows [1, 5] grew to
[1, 5, 2, 3] — non-ascending order that is invalid SpreadsheetML and makes
Excel repair (or drop) the sheet.
"""

from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
SCRIPT = Path(__file__).with_name("xlsx_add_column.py")
ET.register_namespace("", NS)


def write_work_dir(path):
    (path / "xl" / "worksheets").mkdir(parents=True)
    (path / "xl" / "_rels").mkdir()
    ET.ElementTree(
        ET.fromstring(
            '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>'
        )
    ).write(path / "[Content_Types].xml", encoding="utf-8", xml_declaration=True)
    ET.ElementTree(
        ET.fromstring(
            f'<workbook xmlns="{NS}" '
            'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
            '<sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>'
        )
    ).write(path / "xl" / "workbook.xml", encoding="utf-8", xml_declaration=True)
    ET.ElementTree(
        ET.fromstring(
            '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
            '<Relationship Id="rId1" '
            'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/'
            'worksheet" Target="/xl/worksheets/sheet1.xml"/></Relationships>'
        )
    ).write(path / "xl" / "_rels" / "workbook.xml.rels", encoding="utf-8", xml_declaration=True)
    ET.ElementTree(
        ET.fromstring(f'<sst xmlns="{NS}" count="0" uniqueCount="0"></sst>')
    ).write(path / "xl" / "sharedStrings.xml", encoding="utf-8", xml_declaration=True)

    root = ET.Element(f"{{{NS}}}worksheet")
    ET.SubElement(root, f"{{{NS}}}dimension", ref="A1:F5")
    data = ET.SubElement(root, f"{{{NS}}}sheetData")
    for r in (1, 5):  # sparse sheet: row 5 exists, rows 2-4 do not
        row = ET.SubElement(data, f"{{{NS}}}row", r=str(r))
        for c in "ABCDEF":
            cell = ET.SubElement(row, f"{{{NS}}}c", r=f"{c}{r}")
            ET.SubElement(cell, f"{{{NS}}}v").text = str(r)
    ET.ElementTree(root).write(
        path / "xl" / "worksheets" / "sheet1.xml", encoding="utf-8", xml_declaration=True
    )


def read_row_order(sheet_path):
    root = ET.parse(sheet_path).getroot()
    return [row.get("r") for row in root.find(f"{{{NS}}}sheetData")]


class AddColumnRowOrderTests(unittest.TestCase):
    def setUp(self):
        self.work = Path(tempfile.mkdtemp())
        write_work_dir(self.work)

    def test_formula_rows_keep_ascending_order(self):
        result = subprocess.run(
            [
                sys.executable, str(SCRIPT), str(self.work), "--col", "G",
                "--formula", "=F{row}*2", "--formula-rows", "2:3",
            ],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(read_row_order(self.work / "xl" / "worksheets" / "sheet1.xml"),
                         ["1", "2", "3", "5"])

    def test_total_row_keeps_ascending_order(self):
        result = subprocess.run(
            [
                sys.executable, str(SCRIPT), str(self.work), "--col", "G",
                "--total-row", "7", "--total-formula", "=SUM(G1:G5)",
            ],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(read_row_order(self.work / "xl" / "worksheets" / "sheet1.xml"),
                         ["1", "5", "7"])


if __name__ == "__main__":
    unittest.main()
