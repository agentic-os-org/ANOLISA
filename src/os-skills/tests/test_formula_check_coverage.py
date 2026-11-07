#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""formula_check must not report PASS when it checked nothing it advertised."""

import json
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

SCRIPT = (
    Path(__file__).resolve().parents[1]
    / "others"
    / "xlsx"
    / "scripts"
    / "formula_check.py"
)

SHARED_GROUP = (
    '<c r="A1"><f t="shared" ref="A1:A2" si="0">B1+1</f><v>2</v></c>'
    '<c r="A2"><f t="shared" si="0"/><v>3</v></c>'
)


def sheet_xml(cells: str) -> str:
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<worksheet xmlns="{NS_MAIN}"><sheetData><row r="1">{cells}</row>'
        "</sheetData></worksheet>"
    )


def build_workbook(path: str, sheets, targets=None) -> str:
    names = [name for name, _ in sheets]
    targets = targets or [f"worksheets/sheet{i}.xml" for i in range(1, len(sheets) + 1)]
    sheet_entries = "".join(
        f'<sheet name="{name}" sheetId="{index}" r:id="rId{index}"/>'
        for index, name in enumerate(names, 1)
    )
    workbook = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL}">'
        f"<sheets>{sheet_entries}</sheets></workbook>"
    )
    relationships = "".join(
        f'<Relationship Id="rId{index}" '
        'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" '
        f'Target="{target}"/>'
        for index, target in enumerate(targets, 1)
    )
    rels = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        f"{relationships}</Relationships>"
    )
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr(
            "[Content_Types].xml",
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
            '<Default Extension="rels" '
            'ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
            '<Default Extension="xml" ContentType="application/xml"/>'
            '</Types>',
        )
        archive.writestr("xl/workbook.xml", workbook)
        archive.writestr("xl/_rels/workbook.xml.rels", rels)
        for index, (_, cells) in enumerate(sheets, 1):
            archive.writestr(f"xl/worksheets/sheet{index}.xml", sheet_xml(cells))
    return path


def run_check(path: str, *extra: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(SCRIPT), path, "--json", *extra],
        capture_output=True, text=True,
    )


class CoverageTests(unittest.TestCase):
    def run_in_temp(self, sheets, targets=None):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = build_workbook(
            str(Path(directory.name) / "book.xlsx"), sheets, targets
        )
        return path

    def parse(self, result, expected_rc):
        self.assertEqual(result.returncode, expected_rc, result.stdout + result.stderr)
        return json.loads(result.stdout)


class SharedFormulaIntegrityTests(CoverageTests):
    def test_valid_group_passes(self):
        path = self.run_in_temp([("Sheet1", SHARED_GROUP)])
        result = run_check(path)
        data = self.parse(result, 0)
        self.assertEqual(data["shared_formula_ranges"], 1)
        self.assertNotIn("orphan_shared_formula", [e["type"] for e in data["errors"]])

    def test_consumer_before_primary_is_valid(self):
        cells = (
            '<c r="A1"><f t="shared" si="0"/><v>3</v></c>'
            '<c r="A2"><f t="shared" ref="A1:A2" si="0">B1+1</f><v>2</v></c>'
        )
        path = self.run_in_temp([("Sheet1", cells)])
        data = self.parse(run_check(path), 0)
        self.assertEqual(data["errors"], [])

    def test_master_without_ref_is_valid(self):
        cells = (
            '<c r="A1"><f t="shared" si="0">B1+1</f><v>2</v></c>'
            '<c r="A2"><f t="shared" si="0"/><v>3</v></c>'
        )
        path = self.run_in_temp([("Sheet1", cells)])
        data = self.parse(run_check(path), 0)
        self.assertNotIn("orphan_shared_formula", [e["type"] for e in data["errors"]])

    def test_same_si_is_scoped_per_sheet(self):
        path = self.run_in_temp(
            [("Sheet1", SHARED_GROUP), ("Sheet2", SHARED_GROUP)]
        )
        data = self.parse(run_check(path), 0)
        self.assertEqual(data["sheets_checked"], ["Sheet1", "Sheet2"])
        self.assertEqual(data["errors"], [])

    def test_orphan_consumer_fails(self):
        path = self.run_in_temp(
            [("Sheet1", '<c r="B1"><f t="shared" si="99"/><v>0</v></c>')]
        )
        data = self.parse(run_check(path), 1)
        orphans = [e for e in data["errors"] if e["type"] == "orphan_shared_formula"]
        self.assertEqual(len(orphans), 1, data)
        self.assertEqual(orphans[0]["si"], "99")


class SheetSelectionTests(CoverageTests):
    def test_unknown_sheet_filter_fails(self):
        path = self.run_in_temp([("Sheet1", '<c r="A1"><v>1</v></c>')])
        data = self.parse(run_check(path, "--sheet", "Missing"), 1)
        self.assertIn("sheet_not_found", [e["type"] for e in data["errors"]])
        self.assertEqual(data["sheets_checked"], [])

    def test_known_sheet_filter_still_passes(self):
        path = self.run_in_temp([("Sheet1", '<c r="A1"><v>1</v></c>')])
        data = self.parse(run_check(path, "--sheet", "Sheet1"), 0)
        self.assertEqual(data["sheets_checked"], ["Sheet1"])

    def test_empty_sheet_filter_fails(self):
        path = self.run_in_temp([("Sheet1", '<c r="A1"><v>1</v></c>')])
        data = self.parse(run_check(path, "--sheet", ""), 1)
        self.assertIn("sheet_not_found", [e["type"] for e in data["errors"]])

    def test_sheet_option_without_value_fails(self):
        path = self.run_in_temp([("Sheet1", '<c r="A1"><v>1</v></c>')])
        result = run_check(path, "--sheet")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("Usage:", result.stdout)

    def test_package_absolute_target_is_checked(self):
        path = self.run_in_temp(
            [("Sheet1", '<c r="A1" t="e"><v>#REF!</v></c>')],
            ["/xl/worksheets/sheet1.xml"],
        )
        data = self.parse(run_check(path), 1)
        self.assertEqual(data["sheets_checked"], ["Sheet1"])
        self.assertIn("error_value", [e["type"] for e in data["errors"]])

        data = self.parse(run_check(path, "--sheet", "Sheet1"), 1)
        self.assertNotIn("sheet_not_found", [e["type"] for e in data["errors"]])

    def test_unresolvable_target_fails_without_filter(self):
        path = self.run_in_temp(
            [("Sheet1", '<c r="A1"><v>1</v></c>')],
            ["worksheets/missing.xml"],
        )
        data = self.parse(run_check(path), 1)
        self.assertIn("no_sheets_checked", [e["type"] for e in data["errors"]])


if __name__ == "__main__":
    unittest.main()
