#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""CLI regressions for cells without an explicit style index."""

import json
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
SCRIPT = Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts/style_audit.py"

CONTENT_TYPES = (
    '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
    '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
    '<Default Extension="xml" ContentType="application/xml"/>'
    '<Override PartName="/xl/workbook.xml" '
    'ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
    '<Override PartName="/xl/styles.xml" '
    'ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>'
    '<Override PartName="/xl/worksheets/sheet1.xml" '
    'ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
    "</Types>"
).encode()
PACKAGE_RELS = (
    '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
    '<Relationship Id="rId1" Target="xl/workbook.xml" '
    'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"/>'
    "</Relationships>"
).encode()

WORKBOOK = (
    f'<workbook xmlns="{NS}" xmlns:r="{REL}">'
    '<sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>'
).encode()
RELS = (
    '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
    '<Relationship Id="rId1" Target="worksheets/sheet1.xml" '
    'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"/>'
    '<Relationship Id="rId2" Target="styles.xml" '
    'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles"/>'
    "</Relationships>"
).encode()


def make_parts(
    *,
    default_font: str,
    default_format: int,
    cell_xml: str,
    row_attributes: str = "",
    columns_xml: str = "",
) -> dict[str, bytes]:
    styles = (
        f'<styleSheet xmlns="{NS}"><fonts count="2">'
        f'<font><color rgb="{default_font}"/></font>'
        '<font><color rgb="00000000"/></font></fonts>'
        '<fills count="2"><fill><patternFill patternType="none"/></fill>'
        '<fill><patternFill patternType="gray125"/></fill></fills>'
        '<borders count="1"><border/></borders>'
        '<cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/>'
        "</cellStyleXfs>"
        '<cellXfs count="2">'
        f'<xf numFmtId="{default_format}" fontId="0" fillId="0" borderId="0"/>'
        '<xf numFmtId="0" fontId="1" fillId="0" borderId="0"/>'
        '</cellXfs><cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/>'
        "</cellStyles></styleSheet>"
    ).encode()
    sheet = (
        f'<worksheet xmlns="{NS}">{columns_xml}<sheetData>'
        f'<row r="1"{row_attributes}>{cell_xml}</row>'
        "</sheetData></worksheet>"
    ).encode()
    return {
        "[Content_Types].xml": CONTENT_TYPES,
        "_rels/.rels": PACKAGE_RELS,
        "xl/styles.xml": styles,
        "xl/workbook.xml": WORKBOOK,
        "xl/_rels/workbook.xml.rels": RELS,
        "xl/worksheets/sheet1.xml": sheet,
    }


def run_cli(root: Path, parts: dict[str, bytes], packed: bool) -> tuple[int, dict]:
    if packed:
        target = root / "fixture.xlsx"
        with zipfile.ZipFile(target, "w") as archive:
            for name, data in parts.items():
                archive.writestr(name, data)
        before = target.read_bytes()
    else:
        target = root / "unpacked"
        for name, data in parts.items():
            path = target / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        before = {name: (target / name).read_bytes() for name in parts}

    result = subprocess.run(
        [sys.executable, "-X", "utf8", str(SCRIPT), str(target), "--json"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=15,
        check=False,
    )
    if packed:
        assert target.read_bytes() == before
    else:
        assert {name: (target / name).read_bytes() for name in parts} == before
    return result.returncode, json.loads(result.stdout)


class TestDefaultCellStyle(unittest.TestCase):
    def test_omitted_s_matches_explicit_zero_for_blue_formula(self) -> None:
        for packed in (True, False):
            with self.subTest(packed=packed), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                results = []
                for explicit in (False, True):
                    style = ' s="0"' if explicit else ""
                    cell = f'<c r="A1"{style}><f>1+1</f><v>2</v></c>'
                    parts = make_parts(default_font="000000FF", default_format=0, cell_xml=cell)
                    results.append(run_cli(root, parts, packed))
                self.assertEqual(results[0], results[1])
                self.assertEqual(results[0][0], 1)
                self.assertEqual(results[0][1]["violations"][0]["type"], "formula_cell_blue_font")

    def test_omitted_s_matches_explicit_zero_for_comma_year(self) -> None:
        for packed in (True, False):
            with self.subTest(packed=packed), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                results = []
                for explicit in (False, True):
                    style = ' s="0"' if explicit else ""
                    cell = f'<c r="A1"{style}><v>2024</v></c>'
                    parts = make_parts(default_font="000000FF", default_format=3, cell_xml=cell)
                    results.append(run_cli(root, parts, packed))
                self.assertEqual(results[0], results[1])
                self.assertEqual(results[0][1]["violations"][0]["type"], "year_with_comma_format")

    def test_healthy_default_zero(self) -> None:
        for packed in (True, False):
            with self.subTest(packed=packed), tempfile.TemporaryDirectory() as temp:
                cell = '<c r="A1"><f>1+1</f><v>2</v></c>'
                parts = make_parts(default_font="00000000", default_format=0, cell_xml=cell)
                code, audit = run_cli(Path(temp), parts, packed)
                self.assertEqual(code, 0)
                self.assertEqual(audit["violations"], [])

    def test_explicit_nonzero_selects_its_own_style(self) -> None:
        for packed in (True, False):
            with self.subTest(packed=packed), tempfile.TemporaryDirectory() as temp:
                cell = '<c r="A1" s="1"><f>1+1</f><v>2</v></c>'
                parts = make_parts(default_font="000000FF", default_format=0, cell_xml=cell)
                code, audit = run_cli(Path(temp), parts, packed)
                self.assertEqual(code, 0)
                self.assertEqual(audit["violations"], [])

    def test_row_and_column_metadata_does_not_override_existing_cells(self) -> None:
        cases = (
            (' s="1" customFormat="1"', ""),
            ("", '<cols><col min="1" max="1" style="1"/></cols>'),
        )
        for packed in (True, False):
            for row_attributes, columns_xml in cases:
                with self.subTest(
                    packed=packed, row=row_attributes
                ), tempfile.TemporaryDirectory() as temp:
                    parts = make_parts(
                        default_font="000000FF",
                        default_format=0,
                        cell_xml='<c r="A1"><f>1+1</f><v>2</v></c>',
                        row_attributes=row_attributes,
                        columns_xml=columns_xml,
                    )
                    code, audit = run_cli(Path(temp), parts, packed)
                    self.assertEqual(code, 1)
                    self.assertEqual(audit["violations"][0]["type"], "formula_cell_blue_font")


if __name__ == "__main__":
    unittest.main()
