#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Inserting a blank or style-only row must not crash after the shift step.

The dimension-update block called max() over the new-row cell columns, which
is empty when no --text/--values/--formula cells are given: the shift step
had already mutated the sheet, so the crash left the work dir half-mutated
(row slot shifted away, new row never inserted) and a retry double-shifted.
"""

from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
SCRIPT = Path(__file__).with_name("xlsx_insert_row.py")
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
    ET.SubElement(root, f"{{{NS}}}dimension", ref="A1:D10")
    data = ET.SubElement(root, f"{{{NS}}}sheetData")
    for r in range(1, 11):
        row = ET.SubElement(data, f"{{{NS}}}row", r=str(r))
        for c in "ABCD":
            cell = ET.SubElement(row, f"{{{NS}}}c", r=f"{c}{r}")
            ET.SubElement(cell, f"{{{NS}}}v").text = str(r)
    ET.ElementTree(root).write(
        path / "xl" / "worksheets" / "sheet1.xml", encoding="utf-8", xml_declaration=True
    )


def read_rows(sheet_path):
    root = ET.parse(sheet_path).getroot()
    return [row.get("r") for row in root.find(f"{{{NS}}}sheetData")]


class InsertRowTests(unittest.TestCase):
    def setUp(self):
        self.work = Path(tempfile.mkdtemp())
        write_work_dir(self.work)

    def test_blank_styled_insert_completes(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), str(self.work), "--at", "6", "--copy-style-from", "5"],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(read_rows(self.work / "xl" / "worksheets" / "sheet1.xml"),
                         [str(r) for r in range(1, 12)])

    def test_value_insert_still_completes(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), str(self.work), "--at", "6", "--text", "A=hello"],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(read_rows(self.work / "xl" / "worksheets" / "sheet1.xml"),
                         [str(r) for r in range(1, 12)])

    def test_widening_cell_updates_dimension(self):
        # A cell beyond the sheet's end column must widen the dimension
        # (A1:D10 -> A1:F11): pins the widened-dimension branch of the
        # max_col/max_col_letter computation alongside the blank-insert path.
        result = subprocess.run(
            [sys.executable, str(SCRIPT), str(self.work), "--at", "6", "--text", "F=wide"],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        root = ET.parse(self.work / "xl" / "worksheets" / "sheet1.xml").getroot()
        dim = root.find(f"{{{NS}}}dimension")
        self.assertEqual(dim.get("ref"), "A1:F11")


if __name__ == "__main__":
    unittest.main()
