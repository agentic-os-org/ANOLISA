#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_insert_row.py worksheet resolution.

Regression test: workbook.xml.rels targets are relative to xl/ (e.g.
"worksheets/sheet1.xml"), but find_ws_path used to join them to the work
dir root, so the worksheet path never existed.
"""

import os
import sys
import tempfile
import unittest

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
if SCRIPTS_DIR not in sys.path:
    sys.path.insert(0, SCRIPTS_DIR)

from xlsx_insert_row import find_ws_path  # noqa: E402

NS_SS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

WORKBOOK_XML = (
    '<?xml version="1.0"?>'
    f'<workbook xmlns="{NS_SS}" xmlns:r="{NS_REL}">'
    '<sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>'
)


def _make_work_dir(target: str) -> str:
    work_dir = tempfile.mkdtemp()
    os.makedirs(os.path.join(work_dir, "xl", "worksheets"))
    os.makedirs(os.path.join(work_dir, "xl", "_rels"))
    with open(os.path.join(work_dir, "xl", "workbook.xml"), "w", encoding="utf-8") as fh:
        fh.write(WORKBOOK_XML)
    rels = (
        '<?xml version="1.0"?>'
        f'<Relationships xmlns="{NS_REL}">'
        '<Relationship Id="rId1" '
        'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" '
        f'Target="{target}"/></Relationships>'
    )
    with open(os.path.join(work_dir, "xl", "_rels", "workbook.xml.rels"), "w", encoding="utf-8") as fh:
        fh.write(rels)
    with open(os.path.join(work_dir, "xl", "worksheets", "sheet1.xml"), "w", encoding="utf-8") as fh:
        fh.write(f'<?xml version="1.0"?><worksheet xmlns="{NS_SS}"><sheetData/></worksheet>')
    return work_dir


class TestFindWsPath(unittest.TestCase):
    def test_relative_target_resolves_under_xl(self):
        work_dir = _make_work_dir("worksheets/sheet1.xml")
        path = find_ws_path(work_dir, "Data")
        self.assertTrue(os.path.isfile(path), f"resolved path does not exist: {path}")
        self.assertTrue(path.startswith(os.path.join(work_dir, "xl")), path)

    def test_package_absolute_target_resolves(self):
        work_dir = _make_work_dir("/xl/worksheets/sheet1.xml")
        path = find_ws_path(work_dir, "Data")
        self.assertTrue(os.path.isfile(path), f"resolved path does not exist: {path}")
        self.assertTrue(path.startswith(os.path.join(work_dir, "xl")), path)


if __name__ == "__main__":
    unittest.main()
