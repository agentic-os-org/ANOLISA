#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Namespace-binding regression tests for the XLSX editors.

`mc:Ignorable` and `mc:Choice/@Requires` name prefixes inside attribute values.
ElementTree only declares namespaces bound to element or attribute names, so a
compatibility-only prefix lost its declaration while the value kept using it:
the edit exited 0 and produced a part that no longer defines the prefix it
references.
"""

import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

sys.dont_write_bytecode = True

NS_SPREADSHEET = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_MC = "http://schemas.openxmlformats.org/markup-compatibility/2006"
NS_AC = "http://schemas.microsoft.com/office/spreadsheetml/2009/9/ac"

SCRIPTS_DIR = os.path.join(
    str(Path(__file__).resolve().parents[1]), "others", "xlsx", "scripts"
)
SHIFT_ROWS = os.path.join(SCRIPTS_DIR, "xlsx_shift_rows.py")

sys.path.insert(0, SCRIPTS_DIR)

# The prefix appears only in mc:Ignorable; no element or attribute is named
# with it, which is exactly the shape ElementTree cannot round-trip.
COMPATIBILITY_PART = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_SPREADSHEET}" xmlns:mc="{NS_MC}" xmlns:x14ac="{NS_AC}"
           mc:Ignorable="x14ac">
  <sheetFormatPr defaultRowHeight="15" x14ac:dyDescent="0.25"/>
  <sheetData><row r="2"><c r="A2"><v>10</v></c></row></sheetData>
</worksheet>
"""

PLAIN_PART = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_SPREADSHEET}">
  <sheetData><row r="2"><c r="A2"><v>10</v></c></row></sheetData>
</worksheet>
"""


def declared_prefixes(path):
    """Maps every prefix the part declares to its URI."""
    return dict(value for _event, value in ET.iterparse(path, events=("start-ns",)))


def load_script(name):
    """Loads a sibling editor script as a module without running it."""
    spec = importlib.util.spec_from_file_location(
        name.replace("-", "_"), os.path.join(SCRIPTS_DIR, f"{name}.py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ShiftRowsCliTest(unittest.TestCase):
    def test_shift_rows_keeps_a_compatibility_only_prefix(self):
        # The reproduction from the report, end to end: the command used to exit
        # 0 while dropping the declaration `mc:Ignorable` still named.
        with tempfile.TemporaryDirectory() as directory:
            sheet = os.path.join(directory, "xl", "worksheets", "sheet1.xml")
            os.makedirs(os.path.dirname(sheet))
            with open(sheet, "w", encoding="utf-8") as handle:
                handle.write(COMPATIBILITY_PART)
            result = subprocess.run(
                [sys.executable, SHIFT_ROWS, directory, "insert", "2", "1"],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(declared_prefixes(sheet).get("x14ac"), NS_AC)
            root = ET.parse(sheet).getroot()
            self.assertEqual(root.get(f"{{{NS_MC}}}Ignorable"), "x14ac")

    def test_a_plain_workbook_gains_no_declarations(self):
        # The control: restoring bindings must not invent a compatibility
        # prefix for a workbook that never declared one.
        with tempfile.TemporaryDirectory() as directory:
            sheet = os.path.join(directory, "xl", "worksheets", "sheet1.xml")
            os.makedirs(os.path.dirname(sheet))
            with open(sheet, "w", encoding="utf-8") as handle:
                handle.write(PLAIN_PART)
            result = subprocess.run(
                [sys.executable, SHIFT_ROWS, directory, "insert", "2", "1"],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            uris = set(declared_prefixes(sheet).values())
            self.assertNotIn(NS_MC, uris)
            self.assertNotIn(NS_AC, uris)
            self.assertIn(NS_SPREADSHEET, uris)


class WriterTest(unittest.TestCase):
    """The other two editors share the same write path; exercise it directly."""

    def assert_prefix_survives(self, script_name):
        module = load_script(script_name)
        with tempfile.TemporaryDirectory() as directory:
            sheet = os.path.join(directory, "sheet1.xml")
            with open(sheet, "w", encoding="utf-8") as handle:
                handle.write(COMPATIBILITY_PART)
            module._write_tree(ET.parse(sheet), sheet)
            self.assertEqual(declared_prefixes(sheet).get("x14ac"), NS_AC)

    def test_insert_row_keeps_a_compatibility_only_prefix(self):
        self.assert_prefix_survives("xlsx_insert_row")

    def test_add_column_keeps_a_compatibility_only_prefix(self):
        self.assert_prefix_survives("xlsx_add_column")


if __name__ == "__main__":
    unittest.main()
