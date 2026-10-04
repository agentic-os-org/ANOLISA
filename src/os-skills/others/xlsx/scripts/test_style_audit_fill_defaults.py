#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for style_audit.py fill patternType handling.

`patternType` is optional in CT_PatternFill and defaults to "none". Real
writers rely on that default: openpyxl writes `<fill><patternFill/></fill>`
for its default fill. The audit used to read the raw attribute and report
fills[0] as corrupted in such workbooks (a hard violation, exit 1), blocking
delivery of a compliant file. A fill that really is not `none` must still be
reported.
"""

import os
import subprocess
import sys
import tempfile
import unittest
import zipfile

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "style_audit.py")

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

# A compliant sheet: one input cell (blue font) holding a number.
WORKSHEET_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <sheetData>
    <row r="1"><c r="A1" s="1"><v>42</v></c></row>
  </sheetData>
</worksheet>
"""

STYLES_TEMPLATE = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="__NS__">
  <fonts count="1"><font><sz val="11"/><color rgb="000000FF"/></font></fonts>
  <fills count="2">__FILLS__</fills>
  <borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders>
  <cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
  <cellXfs count="2">
    <xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>
    <xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0" applyFont="1"/>
  </cellXfs>
</styleSheet>
"""

# Exactly what openpyxl writes: fills[0] omits patternType (default "none").
DEFAULT_FILLS = '<fill><patternFill/></fill><fill><patternFill patternType="gray125"/></fill>'
# A genuinely wrong fills[0] that must keep failing the audit.
SOLID_FILLS = '<fill><patternFill patternType="solid"/></fill><fill><patternFill patternType="gray125"/></fill>'


def build_xlsx(path: str, fills: str) -> str:
    styles = STYLES_TEMPLATE.replace("__NS__", NS_MAIN).replace("__FILLS__", fills)
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("xl/workbook.xml", WORKBOOK_XML)
        z.writestr("xl/_rels/workbook.xml.rels", RELS_XML)
        z.writestr("xl/styles.xml", styles)
        z.writestr("xl/worksheets/sheet1.xml", WORKSHEET_XML)
    return path


def run_audit(path: str) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, SCRIPT, path],
                          capture_output=True, text=True)


class TestFillPatternTypeDefault(unittest.TestCase):
    def test_missing_pattern_type_defaults_to_none(self):
        """`<patternFill/>` is the OOXML default `none`; it must not be a violation."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "openpyxl-style.xlsx"), DEFAULT_FILLS)
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 0,
                             f"compliant workbook rejected:\n{result.stdout}")
            self.assertNotIn("fills[0] patternType", result.stdout)

    def test_non_none_first_fill_is_still_reported(self):
        """A solid fills[0] is still a real violation."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "solid.xlsx"), SOLID_FILLS)
            result = run_audit(xlsx)
            self.assertEqual(result.returncode, 1, result.stdout)
            self.assertIn("fills[0] patternType", result.stdout)


if __name__ == "__main__":
    unittest.main()
