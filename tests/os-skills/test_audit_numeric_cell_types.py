#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Financial numeric checks must honor the SpreadsheetML cell data type."""

import importlib.util
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts/style_audit.py"
SPEC = importlib.util.spec_from_file_location("style_audit", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
audit = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(audit)
NS = audit.NS
STYLES = (
    f'<styleSheet xmlns="{NS}"><fonts count="2"><font><color rgb="00000000"/></font>'
    '<font><color rgb="000000ff"/></font></fonts><fills count="2">'
    '<fill><patternFill patternType="none"/></fill>'
    '<fill><patternFill patternType="gray125"/></fill></fills>'
    '<borders count="1"><border/></borders><cellXfs count="3">'
    '<xf numFmtId="3" fontId="0" fillId="0" borderId="0"/>'
    '<xf numFmtId="9" fontId="0" fillId="0" borderId="0"/>'
    '<xf numFmtId="3" fontId="1" fillId="0" borderId="0"/>'
    "</cellXfs></styleSheet>"
).encode()


def inspect(cell_xml: str) -> dict:
    sheet = (
        f'<worksheet xmlns="{NS}"><sheetData><row r="1">{cell_xml}' "</row></sheetData></worksheet>"
    ).encode()
    return audit._audit(STYLES, [("Sheet1", sheet)])


class NumericCellTypeTests(unittest.TestCase):
    def test_shared_string_indices_do_not_trigger_numeric_findings(self) -> None:
        for style in (0, 1):
            with self.subTest(style=style):
                result = inspect(f'<c r="A1" s="{style}" t="s"><v>2024</v></c>')
                self.assertEqual(result["violations"], [])
                self.assertEqual(result["warnings"], [])

    def test_cached_string_formulas_do_not_trigger_numeric_findings(self) -> None:
        for style in (0, 1):
            with self.subTest(style=style):
                result = inspect(f'<c r="A1" s="{style}" t="str"><f>"2024"</f><v>2024</v></c>')
                self.assertEqual(result["violations"], [])
                self.assertEqual(result["warnings"], [])

    def test_boolean_and_inline_string_cells_are_not_numeric_inputs(self) -> None:
        for cell in (
            '<c r="A1" s="0" t="b"><v>1</v></c>',
            '<c r="A1" s="0" t="inlineStr"><is><t>2024</t></is></c>',
            '<c r="A1" s="0" t="e"><v>#VALUE!</v></c>',
        ):
            with self.subTest(cell=cell):
                result = inspect(cell)
                self.assertEqual(result["violations"], [])
                self.assertEqual(result["warnings"], [])

    def test_default_and_explicit_numeric_types_keep_the_checks(self) -> None:
        for data_type in ("", ' t="n"'):
            with self.subTest(data_type=data_type):
                result = inspect(f'<c r="A1" s="0"{data_type}><v>2024</v></c>')
                self.assertEqual(result["violations"][0]["type"], "year_with_comma_format")
                self.assertEqual(result["warnings"][0]["type"], "numeric_input_may_lack_blue")
                percentage = inspect(f'<c r="A1" s="1"{data_type}><v>8</v></c>')
                self.assertIn(
                    "percent_value_gt_1", {item["type"] for item in percentage["warnings"]}
                )

    def test_formula_color_checks_still_apply_to_string_formulas(self) -> None:
        result = inspect('<c r="A1" s="2" t="str"><f>"2024"</f><v>2024</v></c>')
        self.assertEqual(
            [item["type"] for item in result["violations"]], ["formula_cell_blue_font"]
        )
        self.assertEqual(result["warnings"], [])


if __name__ == "__main__":
    unittest.main()
