#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_insert_row.py first-text-cell handling.

A workbook without any shared strings is valid OOXML — openpyxl writes
inline strings, so its files carry no xl/sharedStrings.xml part.
Inserting the first text cell used to crash with a raw FileNotFoundError
instead of creating the part together with its package registrations
(the [Content_Types].xml override and the workbook relationship).
"""

import os
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

sys.dont_write_bytecode = True

SCRIPTS = Path(__file__).resolve().parents[1] / "others" / "xlsx" / "scripts"
SCRIPT = str(SCRIPTS / "xlsx_insert_row.py")
UNPACK = str(SCRIPTS / "xlsx_unpack.py")
PACK = str(SCRIPTS / "xlsx_pack.py")

NS_SS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_CT = "http://schemas.openxmlformats.org/package/2006/content-types"
NS_PKG_REL = "http://schemas.openxmlformats.org/package/2006/relationships"
SS_PART_NAME = "/xl/sharedStrings.xml"
SS_CONTENT_TYPE = (
    "application/vnd.openxmlformats-officedocument.spreadsheetml."
    "sharedStrings+xml"
)
SS_REL_TYPE = (
    "http://schemas.openxmlformats.org/officeDocument/2006/"
    "relationships/sharedStrings"
)

try:
    import openpyxl  # noqa: F401
except ImportError:
    openpyxl = None


def make_numeric_workbook(path):
    """A valid workbook with numbers only: no sharedStrings part exists."""
    wb = openpyxl.Workbook()
    ws = wb.active
    for i in range(3):
        ws.cell(row=i + 1, column=1, value=(i + 1) * 10)
        ws.cell(row=i + 1, column=2, value=(i + 1) * 20)
    wb.save(path)


def run_insert(work, at, text):
    return subprocess.run(
        [sys.executable, SCRIPT, work, "--at", str(at), "--text", text],
        capture_output=True, text=True,
    )


def overrides_of(ct_path):
    root = ET.parse(ct_path).getroot()
    return {
        o.get("PartName"): o.get("ContentType")
        for o in root
        if o.tag == f"{{{NS_CT}}}Override"
    }


def ss_rels_of(rels_path):
    root = ET.parse(rels_path).getroot()
    return [rel for rel in root if rel.get("Type") == SS_REL_TYPE]


def after_xml_declaration(content):
    """Strip a leading <?xml ...?> so the root element can be asserted."""
    stripped = content.lstrip()
    if stripped.startswith("<?xml"):
        return stripped[stripped.index("?>") + 2 :].lstrip()
    return stripped


@unittest.skipUnless(openpyxl is not None, "openpyxl is not installed")
class XlsxInsertRowSharedStringsTest(unittest.TestCase):

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.work = os.path.join(self.tmp.name, "work")
        fixture = os.path.join(self.tmp.name, "numeric.xlsx")
        make_numeric_workbook(fixture)
        unpacked = subprocess.run(
            [sys.executable, UNPACK, fixture, self.work],
            capture_output=True, text=True,
        )
        self.assertEqual(unpacked.returncode, 0, unpacked.stderr)
        self.assertFalse(
            os.path.exists(os.path.join(self.work, "xl", "sharedStrings.xml"))
        )

    def tearDown(self):
        self.tmp.cleanup()

    def test_first_text_cell_creates_the_shared_strings_part(self):
        result = run_insert(self.work, 2, "A=hello")
        self.assertEqual(result.returncode, 0, result.stderr)

        ss_path = os.path.join(self.work, "xl", "sharedStrings.xml")
        self.assertTrue(os.path.exists(ss_path), "sharedStrings.xml not created")
        with open(ss_path, encoding="utf-8") as handle:
            content = handle.read()
        self.assertIn("hello", content)

        ct_path = os.path.join(self.work, "[Content_Types].xml")
        with open(ct_path, encoding="utf-8") as handle:
            ct_content = handle.read()
        # the rewrite goes through _write_tree: declaration kept, root
        # unprefixed, entries indented
        self.assertTrue(
            ct_content.lstrip().startswith("<?xml"),
            "the [Content_Types].xml rewrite must keep the XML declaration",
        )
        self.assertTrue(
            after_xml_declaration(ct_content).startswith("<Types xmlns=")
        )
        self.assertEqual(
            overrides_of(ct_path).get(SS_PART_NAME), SS_CONTENT_TYPE
        )

        rels_path = os.path.join(self.work, "xl", "_rels", "workbook.xml.rels")
        with open(rels_path, encoding="utf-8") as handle:
            rels_content = handle.read()
        self.assertTrue(
            rels_content.lstrip().startswith("<?xml"),
            "the workbook.xml.rels rewrite must keep the XML declaration",
        )
        self.assertTrue(
            after_xml_declaration(rels_content).startswith(
                "<Relationships xmlns="
            )
        )
        self.assertEqual(len(ss_rels_of(rels_path)), 1)

    def test_serialization_keeps_the_default_namespace(self):
        """The bootstrap must not leak other parts' default namespaces.

        ET.register_namespace is process-global: creating the part
        switches the default to the content-types and relationship
        vocabularies, and a missing restore degrades every later write
        to <ns0:worksheet>/<ns0:si>. The skill's docs grep for the
        unprefixed forms, so both parts must serialize canonically.
        """
        result = run_insert(self.work, 2, "A=hello")
        self.assertEqual(result.returncode, 0, result.stderr)

        ss_path = os.path.join(self.work, "xl", "sharedStrings.xml")
        with open(ss_path, encoding="utf-8") as handle:
            ss_content = handle.read()
        self.assertEqual(ET.parse(ss_path).getroot().tag, f"{{{NS_SS}}}sst")
        self.assertTrue(
            after_xml_declaration(ss_content).startswith("<sst xmlns=")
        )
        # the grep-based doc contract: unprefixed <si> entries
        self.assertIn("<si>", ss_content)
        self.assertNotIn("ns0:", ss_content)

        ws_path = os.path.join(self.work, "xl", "worksheets", "sheet1.xml")
        with open(ws_path, encoding="utf-8") as handle:
            ws_content = handle.read()
        self.assertEqual(ET.parse(ws_path).getroot().tag, f"{{{NS_SS}}}worksheet")
        self.assertTrue(
            after_xml_declaration(ws_content).startswith("<worksheet xmlns=")
        )
        self.assertNotIn("ns0:", ws_content)

    def test_repacked_workbook_opens_and_reads_the_inserted_text(self):
        result = run_insert(self.work, 2, "A=hello")
        self.assertEqual(result.returncode, 0, result.stderr)
        packed = os.path.join(self.tmp.name, "packed.xlsx")
        repacked = subprocess.run(
            [sys.executable, PACK, self.work, packed],
            capture_output=True, text=True,
        )
        self.assertEqual(repacked.returncode, 0, repacked.stderr)

        wb = openpyxl.load_workbook(packed)
        ws = wb.active
        self.assertEqual(ws["A2"].value, "hello")
        # The insert at row 2 pushed the old rows down: A3 now holds old row
        # 2's value and B4 old row 3's.
        self.assertEqual(ws["A3"].value, 20)
        self.assertEqual(ws["B4"].value, 60)

    def test_half_state_package_self_heals(self):
        """A part without its registrations is equally corrupt in Excel.

        A package can carry xl/sharedStrings.xml yet lack the
        [Content_Types].xml override or the workbook relationship; the
        registrations must heal on the next insert, not short-circuit
        away behind the part-exists check.
        """
        result = run_insert(self.work, 2, "A=hello")
        self.assertEqual(result.returncode, 0, result.stderr)

        ct_path = os.path.join(self.work, "[Content_Types].xml")
        ct_tree = ET.parse(ct_path)
        ct_root = ct_tree.getroot()
        for override in list(ct_root):
            if override.get("PartName") == SS_PART_NAME:
                ct_root.remove(override)
        ct_tree.write(ct_path, encoding="unicode", xml_declaration=False)

        rels_path = os.path.join(self.work, "xl", "_rels", "workbook.xml.rels")
        rels_tree = ET.parse(rels_path)
        rels_root = rels_tree.getroot()
        for rel in list(rels_root):
            if rel.get("Type") == SS_REL_TYPE:
                rels_root.remove(rel)
        rels_tree.write(rels_path, encoding="unicode", xml_declaration=False)

        self.assertIsNone(overrides_of(ct_path).get(SS_PART_NAME))
        self.assertEqual(len(ss_rels_of(rels_path)), 0)

        result = run_insert(self.work, 3, "A=world")
        self.assertEqual(result.returncode, 0, result.stderr)

        self.assertEqual(
            overrides_of(ct_path).get(SS_PART_NAME), SS_CONTENT_TYPE
        )
        self.assertEqual(len(ss_rels_of(rels_path)), 1)

        packed = os.path.join(self.tmp.name, "packed.xlsx")
        repacked = subprocess.run(
            [sys.executable, PACK, self.work, packed],
            capture_output=True, text=True,
        )
        self.assertEqual(repacked.returncode, 0, repacked.stderr)
        wb = openpyxl.load_workbook(packed)
        self.assertEqual(wb.active["A2"].value, "hello")
        self.assertEqual(wb.active["A3"].value, "world")

    def test_second_run_does_not_duplicate_registrations(self):
        result = run_insert(self.work, 2, "A=hello")
        self.assertEqual(result.returncode, 0, result.stderr)
        result = run_insert(self.work, 3, "A=world")
        self.assertEqual(result.returncode, 0, result.stderr)

        ct_path = os.path.join(self.work, "[Content_Types].xml")
        self.assertEqual(
            overrides_of(ct_path).get(SS_PART_NAME), SS_CONTENT_TYPE
        )
        rels_path = os.path.join(self.work, "xl", "_rels", "workbook.xml.rels")
        self.assertEqual(len(ss_rels_of(rels_path)), 1)

        ss_path = os.path.join(self.work, "xl", "sharedStrings.xml")
        with open(ss_path, encoding="utf-8") as handle:
            self.assertEqual(handle.read().count("<si>"), 2)


if __name__ == "__main__":
    unittest.main()
