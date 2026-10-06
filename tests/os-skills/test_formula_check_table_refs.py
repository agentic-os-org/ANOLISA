#!/usr/bin/env python3
"""Regression tests for formula_check.py structured table references.

Excel structured references (\`=SUM(Sales[Amount])\`,
\`=Sales[[#Totals],[Amount]]\`) name tables and columns that live in
\`xl/tables/*.xml\`, not in the workbook's definedNames. The name-ref
extractor read both the table name and every column name inside the
brackets as unknown named ranges, so any workbook using tables failed
validation with false \`unknown_name_ref\` errors.
"""

import importlib.util
import unittest
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src"
    / "os-skills"
    / "others"
    / "xlsx"
    / "scripts"
    / "formula_check.py"
)


def load_module():
    spec = importlib.util.spec_from_file_location("formula_check_under_test", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class StructuredTableRefsNotNames(unittest.TestCase):
    def setUp(self):
        self.module = load_module()

    def test_simple_table_ref_yields_no_names(self):
        self.assertEqual(self.module.extract_name_refs("=SUM(Sales[Amount])"), [])
        self.assertEqual(self.module.extract_name_refs("=AVERAGE(Table1[Column1])"), [])

    def test_nested_table_ref_yields_no_names(self):
        self.assertEqual(
            self.module.extract_name_refs("=SUM(Sales[[#Totals],[Amount]])"), []
        )
        self.assertEqual(
            self.module.extract_name_refs("=Sales[[#This Row],[Units]]*[Price]"), []
        )

    def test_table_ref_mixed_with_real_name(self):
        # A genuine named range next to a table ref is still extracted.
        self.assertEqual(
            self.module.extract_name_refs("=SUM(Sales[Amount])+tax_rate"),
            ["tax_rate"],
        )

    def test_plain_name_still_extracted(self):
        self.assertIn("revenue_total", self.module.extract_name_refs("=revenue_total*2"))


if __name__ == "__main__":
    unittest.main()
