#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Regression tests for the shared string table's character handling.

A string carrying a control character used to be written into the table
verbatim: the builder exited 0 and produced a sharedStrings.xml that every XML
parser rejects, so the workbook could no longer be opened.
"""

import importlib.util
import os
import subprocess
import sys
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

sys.dont_write_bytecode = True

SCRIPTS_DIR = os.path.join(
    str(Path(__file__).resolve().parents[1]), "others", "xlsx", "scripts"
)
SCRIPT = os.path.join(SCRIPTS_DIR, "shared_strings_builder.py")
NS_SST = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"


def build(strings):
    """Runs the builder the way a caller does and returns its stdout."""
    result = subprocess.run(
        [sys.executable, SCRIPT, *strings], capture_output=True, text=True, check=False
    )
    if result.returncode != 0:
        raise AssertionError(result.stderr)
    return result.stdout


def build_directly(strings):
    """Renders in-process, for a payload a command line cannot carry."""
    spec = importlib.util.spec_from_file_location("shared_strings_builder", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.build_xml(module.deduplicate(strings))


def entries(xml_text):
    """Returns the table's string payloads, in order."""
    return [entry.text or "" for entry in ET.fromstring(xml_text).iter(f"{{{NS_SST}}}t")]


class ControlCharacterTest(unittest.TestCase):
    def test_every_unstorable_character_produces_a_parseable_table(self):
        # 0x09 (tab), 0x0a (newline) and 0x0d (carriage return) are reachable in
        # XML or normalised by parsers; the rest are not, and used to break it.
        unstorable = [code for code in range(0x01, 0x20) if code not in (0x09, 0x0A, 0x0D)]
        for code in unstorable:
            with self.subTest(code=code):
                payload = entries(build([f"before{chr(code)}after"]))
                self.assertEqual(payload, [f"before_x{code:04X}_after"])
        # A command line cannot carry NUL, so the same rule is checked in-process.
        self.assertEqual(
            entries(build_directly(["before\x00after"])), ["before_x0000_after"]
        )

    def test_a_stored_escape_is_not_read_back_as_a_control_character(self):
        # `_x000B_` in the text is a literal, not a vertical tab; the format
        # escapes the leading `_x` so the two cannot be confused.
        self.assertEqual(entries(build(["_x000B_"])), ["_x005F_x000B_"])

    def test_ordinary_text_is_unchanged(self):
        payload = entries(build(["Revenue", "a & b < c", "  padded  "]))
        self.assertEqual(payload, ["Revenue", "a & b < c", "  padded  "])


if __name__ == "__main__":
    unittest.main()
