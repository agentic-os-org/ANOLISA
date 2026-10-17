#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Invalid worksheet selection must leave an unpacked workbook unchanged."""

import os
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts/xlsx_insert_row.py"
)
NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"


class WorksheetPreflightTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.work_dir = Path(self.temp_dir.name)
        (self.work_dir / "xl/_rels").mkdir(parents=True)
        (self.work_dir / "xl/worksheets").mkdir()
        (self.work_dir / "xl/workbook.xml").write_text(
            f'<workbook xmlns="{NS}" xmlns:r="{REL}"><sheets>'
            '<sheet name="Good" sheetId="1" r:id="rId1"/>'
            '<sheet name="Other" sheetId="2" r:id="rId2"/>'
            "</sheets></workbook>",
            encoding="utf-8",
        )
        self.rels_path = self.work_dir / "xl/_rels/workbook.xml.rels"
        self.rels_path.write_text(
            '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
            '<Relationship Id="rId1" Target="/xl/worksheets/sheet1.xml"/>'
            '<Relationship Id="rId2" Target="/xl/worksheets/sheet2.xml"/>'
            "</Relationships>",
            encoding="utf-8",
        )
        for name in ("sheet1.xml", "sheet2.xml"):
            (self.work_dir / "xl/worksheets" / name).write_text(
                f'<worksheet xmlns="{NS}"><dimension ref="A1:A2"/><sheetData>'
                '<row r="1"><c r="A1"><v>1</v></c></row>'
                '<row r="2"><c r="A2"><v>2</v></c></row>'
                "</sheetData></worksheet>",
                encoding="utf-8",
            )

    def snapshot(self) -> dict[str, bytes]:
        return {
            str(path.relative_to(self.work_dir)): path.read_bytes()
            for path in self.work_dir.rglob("*")
            if path.is_file()
        }

    def run_insert(self, sheet: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                str(self.work_dir),
                "--at",
                "2",
                "--sheet",
                sheet,
                "--values",
                "A=7",
            ],
            capture_output=True,
            text=True,
            encoding="utf-8",
            env=dict(os.environ, PYTHONIOENCODING="utf-8"),
            cwd=self.work_dir,
        )

    def assert_rejected_unchanged(self, sheet: str) -> None:
        before = self.snapshot()
        result = self.run_insert(sheet)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.snapshot(), before, result.stdout + result.stderr)

    def test_unknown_sheet_does_not_shift_any_worksheet(self) -> None:
        self.assert_rejected_unchanged("DoesNotExist")

    def test_missing_selected_part_does_not_shift_any_worksheet(self) -> None:
        self.rels_path.write_text(
            self.rels_path.read_text(encoding="utf-8").replace("sheet2.xml", "missing.xml"),
            encoding="utf-8",
        )
        self.assert_rejected_unchanged("Other")

    def test_missing_relationship_does_not_shift_any_worksheet(self) -> None:
        tree = ET.parse(self.rels_path)
        tree.getroot().remove(tree.getroot()[1])
        tree.write(self.rels_path, encoding="utf-8")
        self.assert_rejected_unchanged("Other")

    def test_malformed_selected_part_does_not_shift_earlier_worksheet(self) -> None:
        (self.work_dir / "xl/worksheets/sheet2.xml").write_text("<worksheet", encoding="utf-8")
        self.assert_rejected_unchanged("Other")

    def test_valid_selection_inserts_values_and_shifts_existing_rows(self) -> None:
        result = self.run_insert("Good")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        root = ET.parse(self.work_dir / "xl/worksheets/sheet1.xml").getroot()
        self.assertEqual([row.get("r") for row in root.find(f"{{{NS}}}sheetData")], ["1", "2", "3"])
        cells = {cell.get("r"): cell.find(f"{{{NS}}}v").text for cell in root.iter(f"{{{NS}}}c")}
        self.assertEqual(cells, {"A1": "1", "A2": "7", "A3": "2"})
        self.assertEqual(root.find(f"{{{NS}}}dimension").get("ref"), "A1:A3")


if __name__ == "__main__":
    unittest.main()
