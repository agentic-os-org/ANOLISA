#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for formula_check.py shared formula integrity.

A consumer cell whose shared-group id has no primary cell with formula
text is corruption (invalid OOXML; Excel shows the file-repair prompt),
yet the checker used to pass such files because consumers were skipped
before any check ran.
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest
import zipfile

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "formula_check.py")

CONTENT_TYPES = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
    '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
    '<Default Extension="xml" ContentType="application/xml"/>'
    '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
    '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
    '</Types>'
)

WORKBOOK_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL}">
  <sheets>
    <sheet name="Data" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>
"""

WB_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
    Target="worksheets/sheet1.xml"/>
</Relationships>
"""


def build_xlsx(path: str, cells: str) -> str:
    worksheet = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<worksheet xmlns="{NS_MAIN}"><sheetData>'
        f'<row r="1">{cells}</row>'
        '</sheetData></worksheet>'
    )
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("[Content_Types].xml", CONTENT_TYPES)
        z.writestr("xl/workbook.xml", WORKBOOK_XML)
        z.writestr("xl/_rels/workbook.xml.rels", WB_RELS)
        z.writestr("xl/worksheets/sheet1.xml", worksheet)
    return path


def run_check(args: list) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, SCRIPT] + args,
                          capture_output=True, text=True)


# Healthy group: primary si=1 with formula text and ref, consumer si=1.
HEALTHY_CELLS = (
    '<c r="A1"><f t="shared" ref="A1:A2" si="1">SUM(B1:B2)</f><v>3</v></c>'
    '<c r="A2"><f t="shared" si="1"/><v>3</v></c>'
)

# Orphan: consumer si=99, no primary for that group anywhere on the sheet.
ORPHAN_CELLS = (
    '<c r="A1"><f t="shared" ref="A1:A2" si="1">SUM(B1:B2)</f><v>3</v></c>'
    '<c r="A2"><f t="shared" si="1"/><v>3</v></c>'
    '<c r="B1"><f t="shared" si="99"/><v>0</v></c>'
)

# Primary declaration without formula text: no cell carries the group's
# formula, so the si=1 consumers have nothing to inherit.
EMPTY_PRIMARY_CELLS = (
    '<c r="A1"><f t="shared" ref="A1:A2" si="1"/><v>0</v></c>'
    '<c r="A2"><f t="shared" si="1"/><v>0</v></c>'
)


class TestSharedFormulaIntegrity(unittest.TestCase):
    def test_healthy_shared_group_passes(self):
        """Primary + matching consumer is a valid group: PASS, exit 0."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "healthy.xlsx"), HEALTHY_CELLS)
            result = run_check([xlsx])
            self.assertEqual(result.returncode, 0, result.stdout)
            self.assertIn("PASS", result.stdout)
            self.assertNotIn("orphan", result.stdout)

    def test_orphan_consumer_si_fails(self):
        """A consumer whose si has no primary must be a hard error, exit 1."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "orphan.xlsx"), ORPHAN_CELLS)
            result = run_check([xlsx])
            self.assertEqual(result.returncode, 1, result.stdout)
            self.assertIn("FAIL", result.stdout)
            self.assertIn("B1", result.stdout)
            self.assertIn("si '99'", result.stdout)

    def test_orphan_consumer_si_json(self):
        """--json reports the orphan as orphan_shared_formula, exit 1."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "orphan.xlsx"), ORPHAN_CELLS)
            result = run_check([xlsx, "--json"])
            self.assertEqual(result.returncode, 1, result.stdout)
            payload = json.loads(result.stdout)
            orphans = [e for e in payload["errors"]
                       if e["type"] == "orphan_shared_formula"]
            self.assertEqual(len(orphans), 1, payload["errors"])
            self.assertEqual(orphans[0]["cell"], "B1")
            self.assertEqual(orphans[0]["si"], "99")

    def test_primary_without_formula_text_fails(self):
        """A primary declaration with empty text leaves consumers orphaned."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "empty_primary.xlsx"),
                              EMPTY_PRIMARY_CELLS)
            result = run_check([xlsx, "--json"])
            self.assertEqual(result.returncode, 1, result.stdout)
            payload = json.loads(result.stdout)
            orphans = [e for e in payload["errors"]
                       if e["type"] == "orphan_shared_formula"]
            self.assertEqual(len(orphans), 1, payload["errors"])
            self.assertEqual(orphans[0]["si"], "1")


if __name__ == "__main__":
    unittest.main()
