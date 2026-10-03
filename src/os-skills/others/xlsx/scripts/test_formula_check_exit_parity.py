#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for formula_check.py exit-code parity across output modes.

Heuristic warnings (unknown_name_ref) alone do not block delivery in the
human mode by design; --json and --report used to exit 1 on the same
file (and --report labelled it errors_found), so the same workbook
passed or failed depending on the output mode an agent picked.
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest
import zipfile

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "formula_check.py")

CONTENT_TYPES = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
    '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
    '<Default Extension="xml" ContentType="application/xml"/>'
    '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
    '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
    '</Types>'
)

WORKBOOK_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL}">
  <sheets>
    <sheet name="Data" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>
"""

WB_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
    Target="worksheets/sheet1.xml"/>
</Relationships>
"""


def build_xlsx(path: str, cell: str) -> str:
    worksheet = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<worksheet xmlns="{NS_MAIN}"><sheetData>'
        f'<row r="1">{cell}</row>'
        '</sheetData></worksheet>'
    )
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("[Content_Types].xml", CONTENT_TYPES)
        z.writestr("xl/workbook.xml", WORKBOOK_XML)
        z.writestr("xl/_rels/workbook.xml.rels", WB_RELS)
        z.writestr("xl/worksheets/sheet1.xml", worksheet)
    return path


def run_check(args: list) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, SCRIPT] + args,
                          capture_output=True, text=True)


# Heuristic warning only: identifier that is neither a defined name nor a
# function call -> unknown_name_ref (a [WARN], not a [FAIL]).
WARN_ONLY_CELL = '<c r="A1"><f>MyUndefinedName*2</f><v>0</v></c>'

# Definitive failure: a cached error value.
HARD_ERROR_CELL = '<c r="A1" t="e"><f>1/0</f><v>#DIV/0!</v></c>'

# Nothing wrong at all.
CLEAN_CELL = '<c r="A1"><f>1+1</f><v>2</v></c>'


class TestExitCodeParity(unittest.TestCase):
    def _modes(self, path):
        return {
            mode: run_check([path] + (["--json"] if mode == "json"
                                      else ["--report"] if mode == "report" else []))
            for mode in ("human", "json", "report")
        }

    def test_warnings_only_exit_zero_everywhere(self):
        """Heuristic-only file: exit 0 in human, --json and --report."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "warn.xlsx"), WARN_ONLY_CELL)
            for mode, result in self._modes(xlsx).items():
                self.assertEqual(result.returncode, 0,
                                 f"{mode}: {result.stdout}{result.stderr}")
            report = json.loads(self._modes(xlsx)["report"].stdout)
            self.assertEqual(report["status"], "warnings_only")
            self.assertEqual(report["total_errors"], 1)

    def test_hard_error_exit_one_everywhere(self):
        """Definitive failure: exit 1 in human, --json and --report."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "hard.xlsx"), HARD_ERROR_CELL)
            for mode, result in self._modes(xlsx).items():
                self.assertEqual(result.returncode, 1,
                                 f"{mode}: {result.stdout}{result.stderr}")
            report = json.loads(self._modes(xlsx)["report"].stdout)
            self.assertEqual(report["status"], "errors_found")

    def test_clean_file_success_everywhere(self):
        """Clean workbook: exit 0 and status success in every mode."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "clean.xlsx"), CLEAN_CELL)
            for mode, result in self._modes(xlsx).items():
                self.assertEqual(result.returncode, 0,
                                 f"{mode}: {result.stdout}{result.stderr}")
            report = json.loads(self._modes(xlsx)["report"].stdout)
            self.assertEqual(report["status"], "success")


if __name__ == "__main__":
    unittest.main()
