#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Keep complete reference styles when adding number formats."""

import importlib.util
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path
from unittest import mock

SCRIPT = (
    Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts/xlsx_add_column.py"
)
SPEC = importlib.util.spec_from_file_location("xlsx_add_column", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
add_column = importlib.util.module_from_spec(SPEC)
with mock.patch.object(sys, "path", [str(SCRIPT.parent), *sys.path]):
    SPEC.loader.exec_module(add_column)
NS = add_column.NS_SS
BASE_ATTRIBUTES = 'fontId="0" fillId="0" borderId="0" xfId="0" applyAlignment="1"'
CHILDREN = '<alignment horizontal="left"/><protection locked="0"/>'


class NumberFormatStyleTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.work_dir = Path(self.temp_dir.name)
        (self.work_dir / "xl").mkdir()
        self.styles_path = self.work_dir / "xl/styles.xml"

    def write_styles(self, existing_style: str) -> None:
        self.styles_path.write_text(
            f'<styleSheet xmlns="{NS}"><numFmts count="1">'
            '<numFmt numFmtId="164" formatCode="0.0%"/></numFmts>'
            f'<cellXfs count="2"><xf numFmtId="0" {BASE_ATTRIBUTES}>'
            f"{CHILDREN}</xf>{existing_style}</cellXfs></styleSheet>",
            encoding="utf-8",
        )

    def assert_reference_clone(self, style_index: int) -> None:
        styles = ET.parse(self.styles_path).find(f"{{{NS}}}cellXfs")
        self.assertEqual(len(styles), 3)
        expected_attributes = dict(styles[0].attrib, numFmtId="164", applyNumberFormat="true")
        self.assertEqual(styles[style_index].attrib, expected_attributes)
        self.assertEqual(styles[style_index].find(f"{{{NS}}}alignment").get("horizontal"), "left")
        self.assertEqual(styles[style_index].find(f"{{{NS}}}protection").get("locked"), "0")
        self.assertEqual(styles[0].get("numFmtId"), "0")
        self.assertEqual(styles.get("count"), "3")

    def test_does_not_reuse_different_alignment(self) -> None:
        self.write_styles(
            f'<xf numFmtId="164" applyNumberFormat="true" {BASE_ATTRIBUTES}>'
            '<alignment horizontal="right"/><protection locked="0"/></xf>'
        )
        self.assert_reference_clone(add_column.ensure_numfmt_style(str(self.work_dir), 0, "0.0%"))

    def test_does_not_reuse_different_protection(self) -> None:
        self.write_styles(
            f'<xf numFmtId="164" applyNumberFormat="true" {BASE_ATTRIBUTES}>'
            '<alignment horizontal="left"/><protection locked="1"/></xf>'
        )
        self.assert_reference_clone(add_column.ensure_numfmt_style(str(self.work_dir), 0, "0.0%"))

    def test_does_not_reuse_different_style_inheritance(self) -> None:
        attributes = BASE_ATTRIBUTES.replace('xfId="0"', 'xfId="1"')
        self.write_styles(
            f'<xf numFmtId="164" applyNumberFormat="true" {attributes}>{CHILDREN}</xf>'
        )
        self.assert_reference_clone(add_column.ensure_numfmt_style(str(self.work_dir), 0, "0.0%"))

    def test_reuses_identical_style_despite_attribute_order_and_indentation(self) -> None:
        self.write_styles(
            f'<xf {BASE_ATTRIBUTES} applyNumberFormat="true" numFmtId="164">\n'
            f"  {CHILDREN}\n</xf>"
        )
        original = self.styles_path.read_bytes()
        self.assertEqual(add_column.ensure_numfmt_style(str(self.work_dir), 0, "0.0%"), 1)
        self.assertEqual(self.styles_path.read_bytes(), original)

    def test_repeated_requests_reuse_the_created_clone(self) -> None:
        self.write_styles(
            f'<xf numFmtId="164" applyNumberFormat="true" {BASE_ATTRIBUTES}>'
            '<alignment horizontal="right"/><protection locked="0"/></xf>'
        )
        first = add_column.ensure_numfmt_style(str(self.work_dir), 0, "0.0%")
        second = add_column.ensure_numfmt_style(str(self.work_dir), 0, "0.0%")
        self.assertEqual(first, second)
        self.assert_reference_clone(second)


if __name__ == "__main__":
    unittest.main()
