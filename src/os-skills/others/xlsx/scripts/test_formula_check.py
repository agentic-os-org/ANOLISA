#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for formula_check.py input handling.

Uncheckable input must be an error, never PASS: a --sheet filter that
matches no sheet used to print the unchecked name as verified and exit 0,
and a valid zip without OOXML parts used to raise a raw KeyError traceback
(zero bytes of JSON in --json mode).
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

# One plain formula: a healthy sheet the happy-path guard expects to pass.
WORKSHEET_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <sheetData>
    <row r="1"><c r="A1"><f>1+1</f><v>2</v></c></row>
  </sheetData>
</worksheet>
"""


def build_xlsx(path: str) -> str:
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("[Content_Types].xml", CONTENT_TYPES)
        z.writestr("xl/workbook.xml", WORKBOOK_XML)
        z.writestr("xl/_rels/workbook.xml.rels", WB_RELS)
        z.writestr("xl/worksheets/sheet1.xml", WORKSHEET_XML)
    return path


def build_plain_zip(path: str, member: str, content: str) -> str:
    with zipfile.ZipFile(path, "w") as z:
        z.writestr(member, content)
    return path


def run_check(args: list) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, SCRIPT] + args,
                          capture_output=True, text=True)


class TestUncheckableInputFailsClosed(unittest.TestCase):
    def test_valid_sheet_still_passes(self):
        """Happy path: an existing sheet name keeps the clean PASS, exit 0."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "ok.xlsx"))
            result = run_check([xlsx, "--sheet", "Data"])
            self.assertEqual(result.returncode, 0, result.stdout)
            self.assertIn("PASS", result.stdout)

    def test_sheet_filter_no_match_fails_closed(self):
        """--sheet Nope must exit 1 naming the filter, not PASS with 0 sheets."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "ok.xlsx"))
            result = run_check([xlsx, "--sheet", "Nope"])
            self.assertEqual(result.returncode, 1, result.stdout)
            self.assertNotIn("PASS", result.stdout)
            self.assertIn("Nope", result.stdout)
            self.assertIn("Data", result.stdout)  # available sheets listed

    def test_sheet_filter_no_match_json(self):
        """--json reports sheet_not_found as a structured error, exit 1."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "ok.xlsx"))
            result = run_check([xlsx, "--sheet", "Nope", "--json"])
            self.assertEqual(result.returncode, 1, result.stdout)
            payload = json.loads(result.stdout)
            self.assertEqual(payload["error_count"], 1)
            self.assertEqual(payload["errors"][0]["type"], "sheet_not_found")
            self.assertEqual(payload["errors"][0]["sheet_filter"], "Nope")
            self.assertEqual(payload["errors"][0]["available_sheets"], ["Data"])

    def test_non_ooxml_zip_json_emits_file_error(self):
        """A valid zip without xl/workbook.xml: JSON out, file_error, exit 1."""
        with tempfile.TemporaryDirectory() as root:
            zipped = build_plain_zip(os.path.join(root, "not_xlsx.zip.xlsx"),
                                     "word/document.xml", "<doc/>")
            result = run_check([zipped, "--json"])
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            payload = json.loads(result.stdout)
            self.assertEqual(payload["error_count"], 1)
            self.assertEqual(payload["errors"][0]["type"], "file_error")

    def test_non_ooxml_zip_human_mode_fails_cleanly(self):
        """Human mode reports a file error instead of a raw traceback."""
        with tempfile.TemporaryDirectory() as root:
            zipped = build_plain_zip(os.path.join(root, "plain.zip.xlsx"),
                                     "hello.txt", "not a spreadsheet")
            result = run_check([zipped])
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertNotIn("Traceback", result.stderr)
            self.assertIn("File error", result.stdout)
            self.assertNotIn("PASS", result.stdout)


if __name__ == "__main__":
    unittest.main()
