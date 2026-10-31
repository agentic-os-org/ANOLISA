#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Workbook editing must preserve whitespace in shared and inline cell text."""

import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path
from types import ModuleType
from unittest import mock

SCRIPTS = Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts"
NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
VALUES = ("first\n\n   \nlast", "  \n \n", "\nfirst\n", "first\r\nlast\rnext", "A & B < C > D")


def load_script(name: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    with mock.patch.object(sys, "path", [str(SCRIPTS), *sys.path]):
        spec.loader.exec_module(module)
    return module


insert_row = load_script("xlsx_insert_row")
add_column = load_script("xlsx_add_column")
shift_rows = load_script("xlsx_shift_rows")


class EditorTextTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.work_dir = Path(self.temp_dir.name)
        (self.work_dir / "xl/worksheets").mkdir(parents=True)

    def check_shared_strings(self, module: ModuleType) -> None:
        path = self.work_dir / "xl/sharedStrings.xml"
        for value in VALUES:
            with self.subTest(value=value):
                path.write_text(f'<sst xmlns="{NS}" count="0" uniqueCount="0"/>', encoding="utf-8")
                self.assertEqual(module.add_shared_string(str(self.work_dir), value), 0)
                root = ET.parse(path).getroot()
                self.assertEqual(root.find(f"{{{NS}}}si/{{{NS}}}t").text, value)
                self.assertEqual(module.add_shared_string(str(self.work_dir), value), 0)
                self.assertEqual(len(ET.parse(path).getroot()), 1)

    def write_inline_sheet(self, value: str) -> Path:
        root = ET.fromstring(
            f'<worksheet xmlns="{NS}"><dimension ref="A1:A2"/><sheetData>'
            '<row r="2"><c r="A2" t="inlineStr"><is><t xml:space="preserve"/>'
            "</is></c></row></sheetData></worksheet>"
        )
        root.find(f".//{{{NS}}}t").text = value
        path = self.work_dir / "xl/worksheets/sheet1.xml"
        path.write_text(
            ET.tostring(root, encoding="unicode").replace("\r", "&#13;"), encoding="utf-8"
        )
        return path

    def test_insert_row_shared_text_roundtrips_exactly(self) -> None:
        self.check_shared_strings(insert_row)

    def test_add_column_shared_text_roundtrips_exactly(self) -> None:
        self.check_shared_strings(add_column)

    def test_row_shift_keeps_existing_inline_text(self) -> None:
        for value in VALUES:
            with self.subTest(value=value):
                path = self.write_inline_sheet(value)
                shift_rows.process_worksheet(str(path), 2, 1)
                root = ET.parse(path).getroot()
                self.assertEqual(root.find(f".//{{{NS}}}t").text, value)
                self.assertEqual(root.find(f".//{{{NS}}}c").get("r"), "A3")

    def test_standalone_shift_runs_from_arbitrary_working_directory(self) -> None:
        value = VALUES[0]
        path = self.write_inline_sheet(value)
        result = subprocess.run(
            [
                sys.executable,
                str(SCRIPTS / "xlsx_shift_rows.py"),
                str(self.work_dir),
                "insert",
                "2",
                "1",
            ],
            cwd=self.work_dir,
            capture_output=True,
            text=True,
            encoding="utf-8",
            env=dict(os.environ, PYTHONIOENCODING="utf-8"),
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(ET.parse(path).find(f".//{{{NS}}}t").text, value)


if __name__ == "__main__":
    unittest.main()
