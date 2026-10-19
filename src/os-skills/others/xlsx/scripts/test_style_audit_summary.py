#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for the style_audit.py summary counters.

Regression tests for blue-font formula cells being counted twice: the
Check C2 branch incremented `formula_cells` for the violation and the
classification block at the end of the cell loop incremented it again,
so a 3-cell sheet reported "3 inspected (3 formula, 1 input)" and the
summary counts no longer partitioned the inspected cells.

Every styled cell must be classified as formula OR input exactly once,
while the C2 violation itself is still reported.
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest
import zipfile

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "style_audit.py")

sys.path.insert(0, SCRIPTS_DIR)
import style_audit  # noqa: E402

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

# xf 0: theme-black font (formula role); xf 1: blue font (input role).
# All declared counts match the actual element counts, so Check A stays quiet
# and the only violation in the fixtures below is the blue-font formula cell.
STYLES_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="{NS_MAIN}">
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
    <xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>
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

CONTENT_TYPES_XML = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
    '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
    '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
    '<Default Extension="xml" ContentType="application/xml"/>'
    '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
    '</Types>'
)


def worksheet_xml(rows):
    """Build a worksheet from a list of (ref, style index, formula, value) tuples."""
    cells = []
    for ref, style, formula, value in rows:
        inner = ""
        if formula is not None:
            inner += f"<f>{formula}</f>"
        if value is not None:
            inner += f"<v>{value}</v>"
        cells.append(f'<c r="{ref}" s="{style}">{inner}</c>')
    return (f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
            f'<worksheet xmlns="{NS_MAIN}"><sheetData>'
            f'<row r="1">{"".join(cells)}</row>'
            f'</sheetData></worksheet>\n')


# A1: blue-font formula (the Check C2 violation), B1: black-font formula,
# C1: blue-font input value.
MIXED_ROWS = [("A1", 1, "B2+1", 101), ("B1", 0, "A1*2", 202), ("C1", 1, None, 5)]
# No blue-font formula cell: the control fixture for the same partition rule.
CLEAN_ROWS = [("A1", 0, "B1+1", 101), ("B1", 1, None, 5)]


def build_xlsx(path, rows):
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("[Content_Types].xml", CONTENT_TYPES_XML)
        z.writestr("xl/workbook.xml", WORKBOOK_XML)
        z.writestr("xl/_rels/workbook.xml.rels", RELS_XML)
        z.writestr("xl/styles.xml", STYLES_XML)
        z.writestr("xl/worksheets/sheet1.xml", worksheet_xml(rows))
    return path


def run_audit(path, *extra):
    return subprocess.run([sys.executable, SCRIPT, path, *extra],
                          capture_output=True, text=True)


class TestStyleAuditSummaryCounts(unittest.TestCase):
    def _audit_summary(self, rows):
        results = style_audit._audit(
            STYLES_XML.encode("utf-8"),
            [("Sheet1", worksheet_xml(rows).encode("utf-8"))],
        )
        return results

    def test_blue_font_formula_cell_counted_once(self):
        """2 formula + 1 input == 3 inspected, violation still reported once."""
        results = self._audit_summary(MIXED_ROWS)
        s = results["summary"]
        self.assertEqual(3, s["total_cells_inspected"])
        self.assertEqual(2, s["formula_cells"])
        self.assertEqual(1, s["input_cells"])
        self.assertEqual(s["total_cells_inspected"],
                         s["formula_cells"] + s["input_cells"])
        blue = [item for item in results["violations"]
                if item["type"] == "formula_cell_blue_font"]
        self.assertEqual(1, len(blue))
        self.assertEqual("A1", blue[0]["cell"])

    def test_counts_partition_without_blue_formula(self):
        """The control sheet keeps partitioning its inspected cells."""
        s = self._audit_summary(CLEAN_ROWS)["summary"]
        self.assertEqual(2, s["total_cells_inspected"])
        self.assertEqual(1, s["formula_cells"])
        self.assertEqual(1, s["input_cells"])

    def test_every_blue_formula_cell_counted_once(self):
        """A sheet of only blue-font formula cells reports one per cell."""
        rows = [("A1", 1, "B1+1", 1), ("B1", 1, "A1+1", 2), ("C1", 1, "A1+B1", 3)]
        s = self._audit_summary(rows)["summary"]
        self.assertEqual(3, s["total_cells_inspected"])
        self.assertEqual(3, s["formula_cells"])
        self.assertEqual(0, s["input_cells"])

    def test_cli_summary_and_json_agree(self):
        """The printed summary and --json both report the partitioned counts."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "mixed.xlsx"), MIXED_ROWS)

            human = run_audit(xlsx)
            self.assertEqual(1, human.returncode, human.stdout)
            self.assertIn("3 inspected  (2 formula, 1 input)", human.stdout)

            as_json = run_audit(xlsx, "--json")
            self.assertEqual(1, as_json.returncode, as_json.stdout)
            s = json.loads(as_json.stdout)["summary"]
            self.assertEqual(3, s["total_cells_inspected"])
            self.assertEqual(2, s["formula_cells"])
            self.assertEqual(1, s["input_cells"])


if __name__ == "__main__":
    unittest.main()
