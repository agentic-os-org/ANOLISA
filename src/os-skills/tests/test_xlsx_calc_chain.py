# SPDX-License-Identifier: MIT
"""Row shifts must drop the stale xl/calcChain.xml from the work dir.

The calculation chain caches formula load order as cell references, so
after xlsx_shift_rows.py moves rows a repacked workbook that still carries
the pre-shift chain makes Excel show the "found unreadable content"
recovery prompt on open. The script therefore deletes the part together
with its two registrations - the [Content_Types].xml Override and the
workbook.xml.rels Relationship - because removing only the file would
leave them dangling and corrupt the package.

Drives the real CLI via subprocess and never imports from the payload
tree, so nothing can write __pycache__/ into the shipped xlsx skill
directory (see tests/test_raw_packaging.py's payload assertion).
"""

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

SCRIPTS_DIR = os.path.abspath(
    os.path.join(os.path.dirname(os.path.abspath(__file__)),
                 os.pardir, "others", "xlsx", "scripts"))
SHIFT_SCRIPT = os.path.join(SCRIPTS_DIR, "xlsx_shift_rows.py")

CONTENT_TYPES = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
  <Override PartName="/xl/calcChain.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.calcChain+xml"/>
</Types>
"""

# Desktop Excel writes package files as one long line; the cleanup must
# handle that form as well as the pretty-printed one above.
CONTENT_TYPES_SINGLE_LINE = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
    '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
    '<Default Extension="xml" ContentType="application/xml"/>'
    '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
    '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
    '<Override PartName="/xl/calcChain.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.calcChain+xml"/>'
    '</Types>'
)

WORKBOOK_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/calcChain" Target="calcChain.xml"/>
</Relationships>
"""

WORKBOOK_RELS_SINGLE_LINE = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
    '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>'
    '<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/calcChain" Target="calcChain.xml"/>'
    '</Relationships>'
)

CALC_CHAIN = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<calcChain xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <c r="B3" i="1"/>
</calcChain>
"""

WORKBOOK = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheets>
    <sheet name="Sheet1" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>
"""

SHEET1 = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1"><c r="A1"><v>10</v></c></row>
    <row r="3"><c r="B3"><f>A1*2</f></c></row>
  </sheetData>
</worksheet>
"""


class CalcChainDropTest(unittest.TestCase):
    """xlsx_shift_rows.py must remove the calc chain part and registrations."""

    def setUp(self):
        self.work = tempfile.mkdtemp(prefix="xlsx_calcchain_")
        self.addCleanup(shutil.rmtree, self.work, True)

    def _write_fixture(self, with_chain, single_line=False):
        os.makedirs(os.path.join(self.work, "xl", "_rels"))
        os.makedirs(os.path.join(self.work, "xl", "worksheets"))
        with open(os.path.join(self.work, "xl", "workbook.xml"), "w",
                  encoding="utf-8") as fh:
            fh.write(WORKBOOK)
        with open(os.path.join(self.work, "xl", "worksheets", "sheet1.xml"),
                  "w", encoding="utf-8") as fh:
            fh.write(SHEET1)
        with open(os.path.join(self.work, "xl", "_rels", "workbook.xml.rels"),
                  "w", encoding="utf-8") as fh:
            fh.write(WORKBOOK_RELS_SINGLE_LINE if single_line else WORKBOOK_RELS)
        if with_chain:
            with open(os.path.join(self.work, "xl", "calcChain.xml"), "w",
                      encoding="utf-8") as fh:
                fh.write(CALC_CHAIN)
            ct = CONTENT_TYPES_SINGLE_LINE if single_line else CONTENT_TYPES
        else:
            ct = (CONTENT_TYPES_SINGLE_LINE if single_line else CONTENT_TYPES
                  ).replace(
                      '<Override PartName="/xl/calcChain.xml" ContentType='
                      '"application/vnd.openxmlformats-officedocument.'
                      'spreadsheetml.calcChain+xml"/>\n', '')
            ct = ct.replace(
                      '<Override PartName="/xl/calcChain.xml" ContentType='
                      '"application/vnd.openxmlformats-officedocument.'
                      'spreadsheetml.calcChain+xml"/>', '')
        with open(os.path.join(self.work, "[Content_Types].xml"), "w",
                  encoding="utf-8") as fh:
            fh.write(ct)

    def _run_shift(self):
        return subprocess.run(
            [sys.executable, SHIFT_SCRIPT, self.work, "insert", "2", "2"],
            capture_output=True, text=True)

    def _read(self, *parts):
        with open(os.path.join(self.work, *parts), encoding="utf-8") as fh:
            return fh.read()

    def test_insert_shift_drops_calc_chain_and_registrations(self):
        self._write_fixture(with_chain=True)
        result = self._run_shift()
        self.assertEqual(result.returncode, 0, result.stderr)

        self.assertFalse(
            os.path.isfile(os.path.join(self.work, "xl", "calcChain.xml")))

        ct = self._read("[Content_Types].xml")
        self.assertNotIn("calcChain", ct)
        self.assertIn('PartName="/xl/workbook.xml"', ct)
        self.assertIn('PartName="/xl/worksheets/sheet1.xml"', ct)
        ET.fromstring(ct)

        rels = self._read("xl", "_rels", "workbook.xml.rels")
        self.assertNotIn("calcChain", rels)
        self.assertIn("worksheets/sheet1.xml", rels)
        ET.fromstring(rels)

        # The shift itself still happened.
        self.assertIn('r="5"', self._read("xl", "worksheets", "sheet1.xml"))
        self.assertIn("calcChain", result.stdout)

    def test_single_line_package_files(self):
        self._write_fixture(with_chain=True, single_line=True)
        result = self._run_shift()
        self.assertEqual(result.returncode, 0, result.stderr)

        self.assertFalse(
            os.path.isfile(os.path.join(self.work, "xl", "calcChain.xml")))
        ct = self._read("[Content_Types].xml")
        self.assertNotIn("calcChain", ct)
        self.assertIn('PartName="/xl/workbook.xml"', ct)
        rels = self._read("xl", "_rels", "workbook.xml.rels")
        self.assertNotIn("calcChain", rels)
        self.assertIn("worksheets/sheet1.xml", rels)
        ET.fromstring(ct)
        ET.fromstring(rels)

    def test_workbook_without_calc_chain_is_untouched(self):
        self._write_fixture(with_chain=False)
        ct_before = self._read("[Content_Types].xml")
        rels_before = self._read("xl", "_rels", "workbook.xml.rels")

        result = self._run_shift()
        self.assertEqual(result.returncode, 0, result.stderr)

        self.assertEqual(ct_before, self._read("[Content_Types].xml"))
        self.assertEqual(rels_before,
                         self._read("xl", "_rels", "workbook.xml.rels"))
        self.assertNotIn("calcChain", result.stdout)

    def test_dangling_registrations_are_tolerated(self):
        # A damaged package whose Override was already stripped by hand:
        # the file and the remaining Relationship must still go, quietly.
        self._write_fixture(with_chain=True)
        ct_path = os.path.join(self.work, "[Content_Types].xml")
        with open(ct_path, encoding="utf-8") as fh:
            ct = fh.read()
        with open(ct_path, "w", encoding="utf-8") as fh:
            fh.write(ct.replace(
                '<Override PartName="/xl/calcChain.xml" ContentType='
                '"application/vnd.openxmlformats-officedocument.'
                'spreadsheetml.calcChain+xml"/>\n', ''))

        result = self._run_shift()
        self.assertEqual(result.returncode, 0, result.stderr)

        self.assertFalse(
            os.path.isfile(os.path.join(self.work, "xl", "calcChain.xml")))
        self.assertNotIn("calcChain", self._read("[Content_Types].xml"))
        self.assertNotIn("calcChain",
                         self._read("xl", "_rels", "workbook.xml.rels"))


if __name__ == "__main__":
    unittest.main()
