#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Inserting a blank row must not fail or mutate the sheet on error."""

import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

SCRIPT = Path(__file__).with_name('xlsx_insert_row.py')
NS = '{http://schemas.openxmlformats.org/spreadsheetml/2006/main}'

WORKBOOK = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
    'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
    '<sheets><sheet name="Budget" sheetId="1" r:id="rId1"/></sheets></workbook>'
)
RELS = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
    '<Relationship Id="rId1" '
    'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" '
    'Target="{target}"/></Relationships>'
)
SHARED_STRINGS = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
    'count="0" uniqueCount="0"></sst>'
)
SHEET = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
    '<dimension ref="A1:D6"/><sheetData>'
    '<row r="1"><c r="A1" s="1"><v>1</v></c></row>'
    '<row r="5"><c r="A5" s="1"><v>5</v></c></row>'
    '<row r="6"><c r="A6" s="1"><v>6</v></c></row>'
    '</sheetData></worksheet>'
)


def make_workbook(root: Path, target: str) -> Path:
    (root / 'xl' / '_rels').mkdir(parents=True)
    (root / 'xl' / 'worksheets').mkdir()
    (root / 'xl' / 'workbook.xml').write_text(WORKBOOK, encoding='utf-8')
    (root / 'xl' / '_rels' / 'workbook.xml.rels').write_text(
        RELS.format(target=target), encoding='utf-8'
    )
    (root / 'xl' / 'sharedStrings.xml').write_text(SHARED_STRINGS, encoding='utf-8')
    sheet = root / 'xl' / 'worksheets' / 'sheet1.xml'
    sheet.write_text(SHEET, encoding='utf-8')
    return sheet


class EmptyRowInsertTests(unittest.TestCase):
    def run_insert(self, root: Path, *extra: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(SCRIPT), str(root), '--at', '6', *extra],
            capture_output=True, text=True, timeout=60,
        )

    def read_rows(self, root: Path):
        tree = ET.parse(root / 'xl' / 'worksheets' / 'sheet1.xml')
        rows = {int(row.get('r')): row for row in tree.getroot().iter(NS + 'row')}
        dimension = tree.getroot().find(NS + 'dimension').get('ref')
        return rows, dimension

    def test_relative_target_copy_style_only_inserts_blank_row(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            make_workbook(root, 'worksheets/sheet1.xml')
            result = self.run_insert(root, '--sheet', 'Budget', '--copy-style-from', '5')
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            rows, dimension = self.read_rows(root)
            self.assertEqual(sorted(rows), [1, 5, 6, 7])
            self.assertEqual(list(rows[6]), [])
            self.assertEqual(rows[7].find(NS + 'c').get('r'), 'A7')
            self.assertEqual(dimension, 'A1:D7')

    def test_absolute_target_value_insert_still_widens_dimension(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            make_workbook(root, '/xl/worksheets/sheet1.xml')
            result = self.run_insert(root, '--sheet', 'Budget', '--values', 'E=99')
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            rows, dimension = self.read_rows(root)
            self.assertEqual(sorted(rows), [1, 5, 6, 7])
            self.assertEqual(rows[6].find(NS + 'c').get('r'), 'E6')
            self.assertEqual(dimension, 'A1:E7')

    def test_unknown_sheet_does_not_shift(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sheet = make_workbook(root, 'worksheets/sheet1.xml')
            before = sheet.read_bytes()
            result = self.run_insert(root, '--sheet', 'Missing', '--values', 'E=99')
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertEqual(sheet.read_bytes(), before)

    def test_unresolvable_target_does_not_shift(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sheet = make_workbook(root, 'worksheets/missing.xml')
            before = sheet.read_bytes()
            result = self.run_insert(root, '--sheet', 'Budget', '--values', 'E=99')
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn('Worksheet not found', result.stdout)
            self.assertEqual(sheet.read_bytes(), before)


if __name__ == '__main__':
    unittest.main()
