#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Invalid row-edit parameters must fail before mutating a workbook."""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts"
NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"


class PositiveRowTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.work = Path(self.temp_dir.name)
        (self.work / "xl/worksheets").mkdir(parents=True)
        (self.work / "xl/_rels").mkdir()
        fixtures = {
            "xl/worksheets/sheet1.xml": f'<worksheet xmlns="{NS}"><sheetData><row r="2"><c r="A2"><v>4</v></c></row></sheetData></worksheet>',
            "xl/workbook.xml": f'<workbook xmlns="{NS}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" r:id="rId1"/></sheets></workbook>',
            "xl/_rels/workbook.xml.rels": '<Relationships><Relationship Id="rId1" Target="/xl/worksheets/sheet1.xml"/></Relationships>',
        }
        for relative, content in fixtures.items():
            (self.work / relative).write_text(content, encoding="utf-8")

    def snapshot(self) -> dict[str, bytes]:
        return {
            path.relative_to(self.work).as_posix(): path.read_bytes()
            for path in self.work.rglob("*")
            if path.is_file()
        }

    def run_script(self, script: str, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, "-X", "utf8", str(SCRIPTS / script), str(self.work), *args],
            capture_output=True,
            encoding="utf-8",
            check=False,
        )

    def assert_rejected(self, script: str, *args: str) -> None:
        before = self.snapshot()
        result = self.run_script(script, *args)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertNotIn("Traceback", result.stdout + result.stderr)
        self.assertEqual(self.snapshot(), before)

    def test_shift_rejects_nonpositive_rows_and_counts_for_both_operations(self) -> None:
        for operation in ("insert", "delete"):
            for at, count in (("0", "1"), ("-3", "1"), ("2", "0"), ("2", "-1")):
                with self.subTest(operation=operation, at=at, count=count):
                    self.assert_rejected("xlsx_shift_rows.py", operation, at, count)

    def test_shift_reports_noninteger_input_without_traceback(self) -> None:
        for at, count in (("first", "1"), ("2", "many")):
            with self.subTest(at=at, count=count):
                self.assert_rejected("xlsx_shift_rows.py", "insert", at, count)

    def test_insert_rejects_nonpositive_target_and_style_source_rows(self) -> None:
        for args in (
            ("--at", "0"),
            ("--at", "-1"),
            ("--at", "2", "--copy-style-from", "0"),
            ("--at", "2", "--copy-style-from", "-1"),
        ):
            with self.subTest(args=args):
                self.assert_rejected("xlsx_insert_row.py", *args, "--values", "A=1")

    def test_positive_shift_keeps_existing_edit_behavior(self) -> None:
        result = self.run_script("xlsx_shift_rows.py", "insert", "2", "1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(
            'r="A3"', (self.work / "xl/worksheets/sheet1.xml").read_text(encoding="utf-8")
        )

    def test_positive_insert_accepts_first_row_boundary(self) -> None:
        result = self.run_script("xlsx_insert_row.py", "--at", "1", "--values", "A=1")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        sheet = (self.work / "xl/worksheets/sheet1.xml").read_text(encoding="utf-8")
        self.assertIn('r="A1"', sheet)
        self.assertIn('r="A3"', sheet)


if __name__ == "__main__":
    unittest.main()
