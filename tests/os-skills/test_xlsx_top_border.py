#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Applying top rules must preserve the other edges of shared borders."""

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


class TopBorderTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.work_dir = Path(self.temp_dir.name)
        (self.work_dir / "xl").mkdir()
        self.styles_path = self.work_dir / "xl/styles.xml"
        self.styles_path.write_text(
            f'<styleSheet xmlns="{NS}"><borders count="2">'
            '<border diagonalUp="1"><left style="thin"><color rgb="FFFF0000"/></left>'
            '<right style="dashed"/><top style="thin"><color rgb="FF008000"/></top>'
            '<bottom style="double"/><diagonal style="dotted"/></border>'
            '<border><left/><right style="thick"/><bottom style="thin"/><diagonal/></border>'
            '</borders><cellXfs count="3">'
            '<xf fontId="0" fillId="0" borderId="0" numFmtId="0"/>'
            '<xf fontId="0" fillId="0" borderId="1" numFmtId="0"/>'
            '<xf fontId="0" fillId="0" borderId="0" numFmtId="0">'
            '<alignment horizontal="right"/></xf></cellXfs></styleSheet>',
            encoding="utf-8",
        )
        self.worksheet = ET.ElementTree(
            ET.fromstring(
                f'<worksheet xmlns="{NS}"><sheetData><row r="2">'
                '<c r="A2" s="0"/><c r="B2" s="1"/><c r="C2" s="2"/></row>'
                '<row r="3"><c r="A3" s="0"/></row></sheetData></worksheet>'
            )
        )
        self.row_map = {int(row.get("r")): row for row in self.worksheet.iter(f"{{{NS}}}row")}

    def apply_top_border(self, row: int = 2) -> None:
        add_column._apply_border_to_row(
            str(self.work_dir),
            "",
            self.worksheet,
            self.worksheet.getroot(),
            self.row_map,
            row,
            "medium",
            "C",
        )

    def test_preserves_edge_styles_colors_and_diagonal_flags(self) -> None:
        self.apply_top_border()
        root = ET.parse(self.styles_path).getroot()
        styles = root.find(f"{{{NS}}}cellXfs")
        borders = root.find(f"{{{NS}}}borders")
        border = borders[int(styles[int(self.row_map[2][0].get("s"))].get("borderId"))]
        self.assertEqual(border.find(f"{{{NS}}}top").get("style"), "medium")
        top_color = border.find(f"{{{NS}}}top/{{{NS}}}color")
        self.assertIsNotNone(top_color)
        self.assertEqual(top_color.get("rgb"), "FF008000")
        self.assertEqual(border.find(f"{{{NS}}}left").get("style"), "thin")
        self.assertEqual(border.find(f"{{{NS}}}left/{{{NS}}}color").get("rgb"), "FFFF0000")
        self.assertEqual(border.find(f"{{{NS}}}right").get("style"), "dashed")
        self.assertEqual(border.find(f"{{{NS}}}bottom").get("style"), "double")
        self.assertEqual(border.find(f"{{{NS}}}diagonal").get("style"), "dotted")
        self.assertEqual(border.get("diagonalUp"), "1")

    def test_uses_each_original_border_and_reuses_shared_border_clones(self) -> None:
        self.apply_top_border()
        root = ET.parse(self.styles_path).getroot()
        styles = root.find(f"{{{NS}}}cellXfs")
        borders = root.find(f"{{{NS}}}borders")
        border_ids = [int(styles[int(cell.get("s"))].get("borderId")) for cell in self.row_map[2]]
        self.assertEqual(border_ids[0], border_ids[2])
        self.assertNotEqual(border_ids[0], border_ids[1])
        self.assertEqual(len(borders), 4)
        self.assertEqual(borders.get("count"), "4")
        self.assertEqual(borders[border_ids[1]].find(f"{{{NS}}}right").get("style"), "thick")
        self.assertEqual(borders[border_ids[1]].find(f"{{{NS}}}bottom").get("style"), "thin")
        self.assertEqual(
            [edge.tag.rsplit("}", 1)[1] for edge in borders[border_ids[1]]],
            ["left", "right", "top", "bottom", "diagonal"],
        )

    def test_original_shared_styles_remain_unchanged_for_other_rows(self) -> None:
        before = ET.parse(self.styles_path).getroot()
        self.apply_top_border()
        after = ET.parse(self.styles_path).getroot()
        for name, count in (("borders", 2), ("cellXfs", 3)):
            for index in range(count):
                self.assertEqual(
                    ET.canonicalize(
                        ET.tostring(before.find(f"{{{NS}}}{name}")[index], encoding="unicode"),
                        strip_text=True,
                    ),
                    ET.canonicalize(
                        ET.tostring(after.find(f"{{{NS}}}{name}")[index], encoding="unicode"),
                        strip_text=True,
                    ),
                )
        self.assertEqual(self.row_map[3][0].get("s"), "0")

    def test_missing_row_leaves_styles_file_byte_identical(self) -> None:
        before = self.styles_path.read_bytes()
        self.apply_top_border(9)
        self.assertEqual(self.styles_path.read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
