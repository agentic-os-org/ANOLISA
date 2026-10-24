#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Explicit false font flags must not exempt numeric inputs from style checks."""

import importlib.util
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts/style_audit.py"
SPEC = importlib.util.spec_from_file_location("style_audit", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
audit = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(audit)
NS = audit.NS


def styles(bold: str) -> bytes:
    return (
        f'<styleSheet xmlns="{NS}"><fonts count="1"><font>{bold}'
        '<color rgb="00000000"/></font></fonts><fills count="2">'
        '<fill><patternFill patternType="none"/></fill>'
        '<fill><patternFill patternType="gray125"/></fill></fills>'
        '<borders count="1"><border/></borders><cellXfs count="1">'
        '<xf numFmtId="1" fontId="0" fillId="0" borderId="0"/>'
        "</cellXfs></styleSheet>"
    ).encode()


SHEET = (
    f'<worksheet xmlns="{NS}"><sheetData><row r="1">'
    '<c r="A1" s="0"><v>123</v></c></row></sheetData></worksheet>'
).encode()


class BoldValueTests(unittest.TestCase):
    def test_false_and_absent_flags_are_not_bold(self) -> None:
        for bold in ('<b val="0"/>', '<b val="false"/>', '<b val=" false "/>', ""):
            with self.subTest(bold=bold):
                self.assertFalse(audit._parse_styles(styles(bold))["fonts"][0]["bold"])

    def test_true_and_default_flags_are_bold(self) -> None:
        for bold in ("<b/>", '<b val="1"/>', '<b val="true"/>', '<b val=" true "/>'):
            with self.subTest(bold=bold):
                self.assertTrue(audit._parse_styles(styles(bold))["fonts"][0]["bold"])

    def test_false_flags_do_not_suppress_numeric_input_warnings(self) -> None:
        for bold, expected_warnings in (
            ("", 1),
            ('<b val="0"/>', 1),
            ('<b val="false"/>', 1),
            ("<b/>", 0),
            ('<b val="true"/>', 0),
        ):
            with self.subTest(bold=bold):
                result = audit._audit(styles(bold), [("Sheet1", SHEET)])
                self.assertEqual(result["violations"], [])
                self.assertEqual(len(result["warnings"]), expected_warnings)
                if expected_warnings:
                    self.assertEqual(result["warnings"][0]["type"], "numeric_input_may_lack_blue")


if __name__ == "__main__":
    unittest.main()
