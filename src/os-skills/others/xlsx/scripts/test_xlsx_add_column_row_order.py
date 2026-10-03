#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_add_column.py row ordering in sheetData.

Regression test: rows created for --formula-rows / --total-row used to be
appended to the END of <sheetData> regardless of their index. On a sparse
sheet (only rows 1 and 10 defined) adding --formula-rows 2:9 produced the
sequence 1, 10, 2, 3, ..., 9 — violating the ECMA-376 ascending-order
requirement for sheetData rows and triggering Excel's content-repair path.
"""

import os
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "xlsx_add_column.py")
NS_SS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"


def _tag(local: str) -> str:
    return f"{{{NS_SS}}}{local}"


def build_work_dir(root: str, existing_rows: list[int]) -> str:
    work_dir = os.path.join(root, "work")
    os.makedirs(os.path.join(work_dir, "xl", "worksheets"), exist_ok=True)
    os.makedirs(os.path.join(work_dir, "xl", "_rels"), exist_ok=True)

    with open(os.path.join(work_dir, "xl", "workbook.xml"), "w", encoding="utf-8") as f:
        f.write(f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
                f'<workbook xmlns="{NS_SS}" xmlns:r="{NS_REL}">'
                f'<sheets><sheet name="Budget" sheetId="1" r:id="rId1"/></sheets>'
                f'</workbook>')

    with open(os.path.join(work_dir, "xl", "_rels", "workbook.xml.rels"), "w", encoding="utf-8") as f:
        f.write('<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
                '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
                '<Relationship Id="rId1"'
                ' Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"'
                ' Target="xl/worksheets/sheet1.xml"/>'
                '</Relationships>')

    max_row = max(existing_rows)
    rows_xml = "".join(
        f'<row r="{r}"><c r="A{r}" t="n"><v>{r * 10}</v></c></row>'
        for r in existing_rows
    )
    with open(os.path.join(work_dir, "xl", "worksheets", "sheet1.xml"), "w", encoding="utf-8") as f:
        f.write(f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
                f'<worksheet xmlns="{NS_SS}"><dimension ref="A1:F{max_row}"/>'
                f'<sheetData>{rows_xml}</sheetData></worksheet>')
    return work_dir


def run_add_column(work_dir: str, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, SCRIPT, work_dir, *args],
                          capture_output=True, text=True)


def sheet_row_order(work_dir: str) -> list[int]:
    tree = ET.parse(os.path.join(work_dir, "xl", "worksheets", "sheet1.xml"))
    sheet_data = tree.getroot().find(_tag("sheetData"))
    return [int(row.get("r")) for row in sheet_data if row.get("r")]


class TestAddColumnRowOrder(unittest.TestCase):
    def test_sparse_sheet_rows_stay_ascending(self):
        """修复前：稀疏表（只有行 1 和 10）补 --formula-rows 2:9 时新行
        被追加到 sheetData 末尾，产出 1,10,2,...,9 的乱序——违反
        ECMA-376 对 sheetData 行升序的要求，Excel 会走内容修复。
        """
        with tempfile.TemporaryDirectory() as root:
            work_dir = build_work_dir(root, [1, 10])
            result = run_add_column(work_dir, "--col", "G",
                                    "--formula", "=F{row}/100",
                                    "--formula-rows", "2:9",
                                    "--total-row", "10",
                                    "--total-formula", "=SUM(G2:G9)")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(sheet_row_order(work_dir), list(range(1, 11)))
            # 新列单元格必须都在（功能不回归）
            tree = ET.parse(os.path.join(work_dir, "xl", "worksheets", "sheet1.xml"))
            refs = {c.get("r") for c in tree.getroot().iter(_tag("c"))}
            for row in range(2, 11):
                self.assertIn(f"G{row}", refs)

    def test_dense_sheet_order_unchanged(self):
        """正常链路保护：全行存在的表加列后行序不变。"""
        with tempfile.TemporaryDirectory() as root:
            work_dir = build_work_dir(root, list(range(1, 11)))
            result = run_add_column(work_dir, "--col", "G",
                                    "--formula", "=F{row}/100",
                                    "--formula-rows", "2:9")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(sheet_row_order(work_dir), list(range(1, 11)))

    def test_rows_beyond_max_appended_in_order(self):
        """既有最大行之后的补行走追加，结果仍为升序。"""
        with tempfile.TemporaryDirectory() as root:
            work_dir = build_work_dir(root, [1, 2])
            result = run_add_column(work_dir, "--col", "G",
                                    "--formula", "=F{row}/100",
                                    "--formula-rows", "3:5")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(sheet_row_order(work_dir), [1, 2, 3, 4, 5])


if __name__ == "__main__":
    unittest.main()
