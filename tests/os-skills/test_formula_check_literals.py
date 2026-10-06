#!/usr/bin/env python3
"""Regression tests for formula_check.py string-literal handling.

Excel string literals inside formulas are data, not references. The
sheet-ref extractor used to read every word before a ``!`` even inside
double quotes (``"Warning: check Q3!"`` → broken-sheet-ref for ``Q3``),
and the name-ref extractor flagged prose identifiers inside literals as
unknown named ranges. Both extractors now strip double-quoted literals
before matching (quoted sheet names use single quotes, so they are
unaffected).
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


class SheetRefsSkipStringLiterals(unittest.TestCase):
    def setUp(self):
        self.module = load_module()

    def test_prose_exclamation_is_not_a_sheet(self):
        extract = self.module.extract_sheet_refs
        self.assertEqual(extract('IF(A1>0,"Yes! Done!",B1)'), [])
        self.assertEqual(extract('="Warning: check Q3!"'), [])
        self.assertEqual(extract('"Total exceeds budget!"'), [])

    def test_real_sheet_refs_still_extracted(self):
        extract = self.module.extract_sheet_refs
        self.assertEqual(extract("SUM(Sheet2!A1:B2)"), ["Sheet2"])
        self.assertEqual(extract("='My Sheet'!A1+'Other Sheet'!C3"), ["My Sheet", "Other Sheet"])
        # Literal next to a real reference
        self.assertEqual(extract('IF(Sheet2!A1>0,"ok! fine",Sheet2!B1)'), ["Sheet2", "Sheet2"])


class NameRefsSkipStringLiterals(unittest.TestCase):
    def setUp(self):
        self.module = load_module()

    def test_prose_identifiers_are_not_names(self):
        extract = self.module.extract_name_refs
        self.assertEqual(extract('="please check revenue_total again"'), [])
        self.assertEqual(extract('IF(A1>0,"tax_rate applies",B1)'), [])

    def test_real_identifiers_still_extracted(self):
        extract = self.module.extract_name_refs
        self.assertIn("revenue_total", extract("=revenue_total*2"))
        # Function calls are still excluded
        self.assertNotIn("SUM", extract("=SUM(A1:A9)"))


if __name__ == "__main__":
    unittest.main()
