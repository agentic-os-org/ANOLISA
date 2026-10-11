#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Structural style findings must include unused fill and border references."""

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts/style_audit.py"
SPEC = importlib.util.spec_from_file_location("style_audit", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
audit = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(audit)
NS = audit.NS
SHEET = f'<worksheet xmlns="{NS}"><sheetData><row r="1"><c r="A1" s="0"/></row></sheetData></worksheet>'.encode()


def styles_xml(fill_id: int = 0, border_id: int = 0, border_count: int = 1) -> bytes:
    return (
        f'<styleSheet xmlns="{NS}"><fonts count="1"><font/></fonts>'
        '<fills count="2"><fill><patternFill patternType="none"/></fill>'
        '<fill><patternFill patternType="gray125"/></fill></fills>'
        f'<borders count="{border_count}"><border/></borders>'
        '<cellXfs count="2"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/>'
        f'<xf numFmtId="0" fontId="0" fillId="{fill_id}" borderId="{border_id}"/>'
        "</cellXfs></styleSheet>"
    ).encode()


class ResourceReferenceTests(unittest.TestCase):
    def test_unused_styles_are_checked_once_without_sheet_context(self) -> None:
        result = audit._audit(styles_xml(fill_id=2, border_id=1), [("Sheet1", SHEET)])
        self.assertEqual(result["summary"]["violations"], 2)
        self.assertEqual(
            {item["type"] for item in result["violations"]},
            {"fill_index_out_of_range", "border_index_out_of_range"},
        )
        for finding in result["violations"]:
            self.assertEqual(finding["cellXfs_index"], 1)
            self.assertNotIn("sheet", finding)

    def test_negative_references_are_rejected(self) -> None:
        result = audit._audit(styles_xml(fill_id=-1, border_id=-1), [("Sheet1", SHEET)])
        self.assertEqual(len(result["violations"]), 2)
        for finding in result["violations"]:
            field = "fillId" if finding["type"].startswith("fill") else "borderId"
            self.assertEqual(finding[field], -1)

    def test_border_count_mismatch_is_reported(self) -> None:
        result = audit._audit(styles_xml(border_count=7), [("Sheet1", SHEET)])
        self.assertEqual(result["violations"][0]["element"], "borders")
        self.assertEqual(result["violations"][0]["declared"], 7)
        self.assertEqual(result["violations"][0]["actual"], 1)

    def test_valid_resource_boundaries_do_not_add_findings(self) -> None:
        result = audit._audit(styles_xml(fill_id=1, border_id=0), [("Sheet1", SHEET)])
        self.assertEqual(result["violations"], [])

    def test_cli_json_and_text_report_same_structural_errors(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            work = Path(temp_dir)
            (work / "xl/_rels").mkdir(parents=True)
            (work / "xl/worksheets").mkdir()
            (work / "xl/styles.xml").write_bytes(styles_xml(fill_id=5, border_id=9))
            (work / "xl/worksheets/sheet1.xml").write_bytes(SHEET)
            (work / "xl/workbook.xml").write_text(
                f'<workbook xmlns="{NS}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
                '<sheets><sheet name="Sheet1" r:id="rId1"/></sheets></workbook>',
                encoding="utf-8",
            )
            (work / "xl/_rels/workbook.xml.rels").write_text(
                '<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml"/></Relationships>',
                encoding="utf-8",
            )
            reports = []
            for flags in (["--json"], []):
                result = subprocess.run(
                    [sys.executable, "-X", "utf8", str(SCRIPT), str(work), *flags],
                    capture_output=True,
                    encoding="utf-8",
                    check=False,
                )
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                reports.append(result.stdout)
            self.assertEqual(json.loads(reports[0])["summary"]["violations"], 2)
            self.assertIn("cellXfs[1]", reports[1])
            self.assertIn("fillId=5", reports[1])
            self.assertIn("borderId=9", reports[1])


if __name__ == "__main__":
    unittest.main()
