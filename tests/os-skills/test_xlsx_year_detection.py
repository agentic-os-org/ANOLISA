#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Year recommendations must not discard fractions or invent missing dates."""

import importlib.util
import unittest
from pathlib import Path
from types import ModuleType

import pandas as pd

SCRIPTS = Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts"
NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
STYLES = (
    f'<styleSheet xmlns="{NS}"><fonts count="1"><font><color theme="1"/></font></fonts>'
    '<fills count="2"><fill><patternFill patternType="none"/></fill>'
    '<fill><patternFill patternType="gray125"/></fill></fills>'
    '<cellXfs count="1"><xf numFmtId="3" fontId="0" fillId="0" borderId="0"/></cellXfs>'
    "</styleSheet>"
).encode()


def load_script(name: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


style_audit = load_script("style_audit")
xlsx_reader = load_script("xlsx_reader")


class YearDetectionTests(unittest.TestCase):
    def year_violations(self, value: str) -> list[dict]:
        sheet = (
            f'<worksheet xmlns="{NS}"><sheetData><row r="1"><c r="A1" s="0">'
            f"<v>{value}</v></c></row></sheetData></worksheet>"
        ).encode()
        results = style_audit._audit(STYLES, [("Sheet1", sheet)])
        return [item for item in results["violations"] if item["type"] == "year_with_comma_format"]

    def test_fractional_amount_is_not_a_year_format_violation(self) -> None:
        self.assertEqual(self.year_violations("2024.75"), [])

    def test_nonfinite_values_do_not_crash_the_style_audit(self) -> None:
        for value in ("1e999", "NaN", "-1e999"):
            with self.subTest(value=value):
                self.assertEqual(self.year_violations(value), [])

    def test_integer_years_still_trigger_comma_format_violations(self) -> None:
        for value in ("2024", "2024.0"):
            with self.subTest(value=value):
                self.assertEqual(len(self.year_violations(value)), 1)

    def test_fractional_year_column_has_no_integer_conversion_recommendation(self) -> None:
        data = pd.DataFrame({"year": [2024.75, 2025.5]})
        findings = xlsx_reader.audit_quality({"Sheet1": data})["Sheet1"]
        self.assertFalse(any(item["type"] == "year_as_float" for item in findings))

    def test_all_missing_years_keep_null_finding_without_year_recommendation(self) -> None:
        data = pd.DataFrame({"year": [float("nan"), float("nan")]})
        findings = xlsx_reader.audit_quality({"Sheet1": data})["Sheet1"]
        self.assertTrue(any(item["type"] == "null_values" for item in findings))
        self.assertFalse(any(item["type"] == "year_as_float" for item in findings))

    def test_nullable_integral_float_years_get_a_lossless_recommendation(self) -> None:
        data = pd.DataFrame({"year": pd.Series([2024.0, None], dtype="Float64")})
        findings = xlsx_reader.audit_quality({"Sheet1": data})["Sheet1"]
        year_findings = [item for item in findings if item["type"] == "year_as_float"]
        self.assertEqual(len(year_findings), 1)
        self.assertIn("Int64", year_findings[0]["note"])
        converted = data["year"].astype("Int64").astype("string")
        self.assertEqual(converted.iloc[0], "2024")
        self.assertTrue(pd.isna(converted.iloc[1]))


if __name__ == "__main__":
    unittest.main()
