#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for style_audit.py cell style resolution.

An OOXML cell without an ``s`` attribute uses ``cellXfs[0]``, the workbook
default. The auditor used to skip those cells entirely, so a formula formatted
by the default cell format passed the audit even when that format carries the
blue input-role font the guide prohibits.
"""

import os
import re
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

sys.dont_write_bytecode = True

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

SCRIPTS_DIR = os.path.join(
    str(Path(__file__).resolve().parents[1]), "others", "xlsx", "scripts"
)
SCRIPT = os.path.join(SCRIPTS_DIR, "style_audit.py")

STYLES_TEMPLATE = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="{ns}">
  <fonts count="2">
    <font><sz val="11"/><color theme="1"/></font>
    <font><sz val="11"/><color rgb="000000FF"/></font>
  </fonts>
  <fills count="2">
    <fill><patternFill patternType="none"/></fill>
    <fill><patternFill patternType="gray125"/></fill>
  </fills>
  <borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders>
  <cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
  <cellXfs count="2">
    <xf numFmtId="0" fontId="{default_font}" fillId="0" borderId="0" xfId="0"/>
    <xf numFmtId="0" fontId="1" fillId="0" borderId="0" xfId="0" applyFont="1"/>
  </cellXfs>
</styleSheet>
"""

WORKBOOK_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL}">
  <sheets>
    <sheet name="Sheet1" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>
"""

RELS_XML = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
    Target="worksheets/sheet1.xml"/>
</Relationships>
"""

CONTENT_TYPES = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
    '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
    '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
    '<Default Extension="xml" ContentType="application/xml"/>'
    '<Override PartName="/xl/workbook.xml" '
    'ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
    "</Types>"
)


def styles_xml_without_cell_xfs(count: int | None = None) -> str:
    """STYLES_TEMPLATE with the ``cellXfs`` element omitted or left empty.

    ``CT_Stylesheet/cellXfs`` is ``minOccurs=0``, so both spellings are legal
    OOXML and neither provides a ``cellXfs[0]`` default to audit.
    """
    replacement = "" if count is None else f'<cellXfs count="{count}"/>'
    return re.sub(
        r'<cellXfs count="2">.*?</cellXfs>',
        replacement,
        STYLES_TEMPLATE.format(ns=NS_MAIN, default_font=0),
        flags=re.S,
    )


def build_xlsx(
    path: str,
    cell: str,
    default_font: int = 0,
    styles: str | None = None,
    row_attrs: str = "",
) -> str:
    """Write a workbook whose only sheet holds *cell* (raw ``<c>`` XML)."""
    worksheet = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        f'<worksheet xmlns="{NS_MAIN}"><sheetData>'
        f'<row r="1"{row_attrs}>{cell}</row>'
        "</sheetData></worksheet>"
    )
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr("[Content_Types].xml", CONTENT_TYPES)
        archive.writestr("xl/workbook.xml", WORKBOOK_XML)
        archive.writestr("xl/_rels/workbook.xml.rels", RELS_XML)
        archive.writestr(
            "xl/styles.xml",
            styles or STYLES_TEMPLATE.format(ns=NS_MAIN, default_font=default_font),
        )
        archive.writestr("xl/worksheets/sheet1.xml", worksheet)
    return path


def run_audit(path: str) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, SCRIPT, path], capture_output=True, text=True)


class TestDefaultCellStyleIsAudited(unittest.TestCase):
    def test_formula_without_style_attribute_uses_the_default_format(self):
        """A formula inheriting the blue default format must be reported."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(
                os.path.join(root, "default_blue.xlsx"),
                '<c r="A1"><f>1+1</f><v>2</v></c>',
                default_font=1,
            )
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("formula cell has blue font", result.stdout)

    def test_explicit_style_index_zero_behaves_the_same(self):
        """The same cell written with s="0" already fails; both spellings agree."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(
                os.path.join(root, "explicit_zero.xlsx"),
                '<c r="A1" s="0"><f>1+1</f><v>2</v></c>',
                default_font=1,
            )
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("formula cell has blue font", result.stdout)

    def test_formula_with_default_black_format_stays_clean(self):
        """A workbook default font that is not blue produces no violation."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(
                os.path.join(root, "default_black.xlsx"),
                '<c r="A1"><f>1+1</f><v>2</v></c>',
                default_font=0,
            )
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("1 inspected", result.stdout)


    def test_absent_cell_xfs_leaves_unstyled_cells_clean(self):
        """No cellXfs element means no default format, so nothing is inherited."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(
                os.path.join(root, "book.xlsx"),
                '<c r="A1"><v>1</v></c>',
                styles=styles_xml_without_cell_xfs(),
            )
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertNotIn("style_index_out_of_range", result.stdout)

    def test_empty_cell_xfs_leaves_unstyled_cells_clean(self):
        """<cellXfs count="0"/> is legal and provides no default either."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(
                os.path.join(root, "book.xlsx"),
                '<c r="A1"><v>1</v></c>',
                styles=styles_xml_without_cell_xfs(count=0),
            )
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertNotIn("style_index_out_of_range", result.stdout)

    def test_cell_inherits_its_row_style(self):
        """A row with customFormat applies s to cells that carry none."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(
                os.path.join(root, "book.xlsx"),
                '<c r="A1"><f>1</f><v></v></c>',
                row_attrs=' s="1" customFormat="1"',
            )
            result = run_audit(xlsx)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_row_style_without_custom_format_is_not_applied(self):
        """The row's s is inert unless customFormat says otherwise."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(
                os.path.join(root, "book.xlsx"),
                '<c r="A1"><f>1</f><v></v></c>',
                row_attrs=' s="1"',
            )
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
