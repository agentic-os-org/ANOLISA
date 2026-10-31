#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Row shifts must keep validation and conditional-format rule sources aligned."""

import importlib.util
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path
from unittest import mock

SCRIPT = (
    Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts/xlsx_shift_rows.py"
)
SPEC = importlib.util.spec_from_file_location("xlsx_shift_rows", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
shift_rows = importlib.util.module_from_spec(SPEC)
with mock.patch.object(sys, "path", [str(SCRIPT.parent), *sys.path]):
    SPEC.loader.exec_module(shift_rows)
NS = shift_rows.NS_MAIN


class RuleFormulaTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.sheet_path = Path(self.temp_dir.name) / "sheet1.xml"
        self.sheet_path.write_text(
            f'<worksheet xmlns="{NS}"><sheetData/>'
            '<conditionalFormatting sqref="A5:A9"><cfRule type="expression" priority="1">'
            '<formula>A5&gt;$B$9</formula><formula>A5="A5"</formula></cfRule>'
            '<cfRule type="colorScale" priority="2"><colorScale>'
            '<cfvo type="formula" val="MIN(C5:C9)"/><cfvo type="num" val="5"/>'
            "</colorScale></cfRule></conditionalFormatting>"
            '<dataValidations count="3"><dataValidation type="decimal" sqref="B5:B9">'
            "<formula1>$D$5</formula1><formula2>$D$9</formula2></dataValidation>"
            '<dataValidation type="list" sqref="C5:C9"><formula1>$E$5:$E$9</formula1>'
            '</dataValidation><dataValidation type="list" sqref="D5:D9">'
            '<formula1>"A5,No"</formula1></dataValidation></dataValidations></worksheet>',
            encoding="utf-8",
        )

    def test_insert_shifts_conditional_format_expressions_and_formula_thresholds(self) -> None:
        shift_rows.process_worksheet(str(self.sheet_path), 5, 2)
        root = ET.parse(self.sheet_path).getroot()
        rule = root.find(f"{{{NS}}}conditionalFormatting")
        self.assertEqual(rule.get("sqref"), "A7:A11")
        self.assertEqual(rule.find(f"{{{NS}}}cfRule/{{{NS}}}formula").text, "A7>$B$11")
        self.assertEqual(rule.findall(f"{{{NS}}}cfRule/{{{NS}}}formula")[1].text, 'A7="A5"')
        self.assertEqual(rule.find(f".//{{{NS}}}cfvo").get("val"), "MIN(C7:C11)")
        self.assertEqual(rule.findall(f".//{{{NS}}}cfvo")[1].get("val"), "5")

    def test_insert_shifts_validation_bounds_and_range_sources(self) -> None:
        shift_rows.process_worksheet(str(self.sheet_path), 5, 2)
        validations = ET.parse(self.sheet_path).find(f"{{{NS}}}dataValidations")
        self.assertEqual(validations[0].get("sqref"), "B7:B11")
        self.assertEqual(validations[0].find(f"{{{NS}}}formula1").text, "$D$7")
        self.assertEqual(validations[0].find(f"{{{NS}}}formula2").text, "$D$11")
        self.assertEqual(validations[1].find(f"{{{NS}}}formula1").text, "$E$7:$E$11")
        self.assertEqual(validations[2].find(f"{{{NS}}}formula1").text, '"A5,No"')

    def test_delete_shifts_rule_sources_with_target_ranges(self) -> None:
        shift_rows.process_worksheet(str(self.sheet_path), 5, -2)
        root = ET.parse(self.sheet_path).getroot()
        self.assertEqual(root.find(f".//{{{NS}}}formula").text, "A3>$B$7")
        self.assertEqual(root.find(f".//{{{NS}}}cfvo").get("val"), "MIN(C3:C7)")
        self.assertEqual(root.find(f".//{{{NS}}}formula1").text, "$D$3")
        self.assertEqual(root.find(f".//{{{NS}}}formula2").text, "$D$7")

    def test_rule_formulas_preserve_tokens_through_real_xml_updates(self) -> None:
        formula = "IF(SHEET5!A5>1E5,LOG10($B$5),SUM(Table5[A5]))"
        for at, delta, shifted_row in ((5, 2, 7), (5, -2, 3), (20, 2, 5)):
            with self.subTest(at=at, delta=delta):
                root = ET.Element(f"{{{NS}}}worksheet")
                ET.SubElement(root, f"{{{NS}}}sheetData")
                formatting = ET.SubElement(root, f"{{{NS}}}conditionalFormatting", sqref="A5:A9")
                rule = ET.SubElement(formatting, f"{{{NS}}}cfRule", type="expression")
                expression = ET.SubElement(rule, f"{{{NS}}}formula")
                expression.text = formula
                scale_rule = ET.SubElement(formatting, f"{{{NS}}}cfRule", type="colorScale")
                color_scale = ET.SubElement(scale_rule, f"{{{NS}}}colorScale")
                ET.SubElement(color_scale, f"{{{NS}}}cfvo", type="formula", val=formula)
                validations = ET.SubElement(root, f"{{{NS}}}dataValidations", count="1")
                validation = ET.SubElement(
                    validations, f"{{{NS}}}dataValidation", type="decimal", sqref="B5:B9"
                )
                for name in ("formula1", "formula2"):
                    ET.SubElement(validation, f"{{{NS}}}{name}").text = formula
                ET.ElementTree(root).write(self.sheet_path, encoding="utf-8")
                before = self.sheet_path.read_bytes()

                shift_rows.process_worksheet(str(self.sheet_path), at, delta)

                updated = ET.parse(self.sheet_path).getroot()
                expected = f"IF(SHEET5!A{shifted_row}>1E5,LOG10($B${shifted_row}),SUM(Table5[A5]))"
                for name in ("formula", "formula1", "formula2"):
                    self.assertEqual(updated.find(f".//{{{NS}}}{name}").text, expected)
                self.assertEqual(updated.find(f".//{{{NS}}}cfvo").get("val"), expected)
                if at == 20:
                    self.assertEqual(self.sheet_path.read_bytes(), before)

    def test_rule_above_edit_stays_byte_identical(self) -> None:
        before = self.sheet_path.read_bytes()
        self.assertEqual(shift_rows.process_worksheet(str(self.sheet_path), 10, 2), 0)
        self.assertEqual(self.sheet_path.read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
