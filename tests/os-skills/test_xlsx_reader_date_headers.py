#!/usr/bin/env python3
"""Exercise date labels through the complete readonly workbook reader CLI.

Issue #9181: complete workbook CLI coverage for date column keys.
The single-row, null and collision cases also require dependency #8117.

Main's recorded exact #8117 baseline: the three added single-row, null and
collision cases fail. The integrated reader passes all seven cases, and
#8117's unchanged four numeric regressions pass separately. These results were
recorded on Windows. #9182 automated review comment separately reports an unchanged-main
baseline of five failures and two controls; that is reviewer evidence, not a
Main execution result.
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime
from pathlib import Path

try:
    import pandas  # noqa: F401

    HAS_PANDAS = True
except ImportError:
    HAS_PANDAS = False

try:
    import openpyxl  # noqa: F401

    HAS_OPENPYXL = True
except ImportError:
    HAS_OPENPYXL = False

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src/os-skills/others/xlsx/scripts/xlsx_reader.py"
)


def reject_json_constant(value: str) -> None:
    """Make Python's JSON decoder reject NaN and Infinity like strict consumers."""
    raise ValueError(f"Non-standard JSON constant: {value}")


@unittest.skipUnless(
    HAS_PANDAS and HAS_OPENPYXL and sys.version_info >= (3, 10),
    "xlsx_reader.py needs pandas, openpyxl and Python >= 3.10 "
    "(production PEP 604 annotations are evaluated at def time)",
)
class TestDateHeaderJson(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.workbook = Path(self.temporary.name) / "forecast.xlsx"

    def write_workbook(
        self, headers: "list[object]", rows: "list[list[object]]"
    ) -> None:
        from openpyxl import Workbook

        workbook = Workbook()
        sheet = workbook.active
        sheet.title = "Forecast"
        sheet.append(headers)
        for row in rows:
            sheet.append(row)
        workbook.save(self.workbook)
        workbook.close()

    def run_reader(self, *options: str) -> "subprocess.CompletedProcess[str]":
        environment = os.environ.copy()
        environment["PYTHONIOENCODING"] = "utf-8"
        before = self.workbook.read_bytes()
        result = subprocess.run(
            [sys.executable, str(SCRIPT), str(self.workbook), *options],
            capture_output=True,
            text=True,
            encoding="utf-8",
            env=environment,
            check=False,
            timeout=30,
        )
        self.assertEqual(
            self.workbook.read_bytes(), before, "Reader modified the source"
        )
        return result

    def test_date_header_in_full_json_report(self) -> None:
        self.write_workbook(
            [datetime(2026, 1, 1), datetime(2026, 2, 1)],
            [[10, 20], [30, 40]],
        )
        result = self.run_reader("--json")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        report = json.loads(result.stdout, parse_constant=reject_json_constant)
        info = report["structure"]["Forecast"]
        first, second = "2026-01-01 00:00:00", "2026-02-01 00:00:00"
        self.assertEqual(info["columns"], [first, second])
        self.assertEqual(
            info["preview"], [{first: 10, second: 20}, {first: 30, second: 40}]
        )
        self.assertEqual(set(info["dtypes"]), {first, second})
        self.assertEqual(report["stats"]["Forecast"][first]["count"], 2.0)
        self.assertEqual(report["stats"]["Forecast"][first]["mean"], 20.0)

    def test_date_header_in_quality_only_json_report(self) -> None:
        self.write_workbook([datetime(2026, 1, 1), "Label"], [[10, "A"], [30, "B"]])
        result = self.run_reader("--json", "--quality")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        report = json.loads(result.stdout, parse_constant=reject_json_constant)
        self.assertEqual(report["stats"], {})
        self.assertEqual(
            report["structure"]["Forecast"]["preview"][0],
            {"2026-01-01 00:00:00": 10, "Label": "A"},
        )
        self.assertEqual(report["quality"]["Forecast"], [])

    def test_string_headers_and_date_values_remain_supported(self) -> None:
        self.write_workbook(
            ["When", "Amount"],
            [[datetime(2026, 1, 1), 10], [datetime(2026, 2, 1), 30]],
        )
        result = self.run_reader("--json")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        report = json.loads(result.stdout, parse_constant=reject_json_constant)
        info = report["structure"]["Forecast"]
        self.assertEqual(info["columns"], ["When", "Amount"])
        self.assertEqual(info["preview"][0]["When"], "2026-01-01 00:00:00")
        self.assertEqual(report["stats"]["Forecast"]["Amount"]["mean"], 20.0)

    def test_date_header_human_report_stays_readonly(self) -> None:
        self.write_workbook([datetime(2026, 1, 1), "Label"], [[10, "A"], [30, "B"]])
        result = self.run_reader()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Sheet: Forecast", result.stdout)
        self.assertIn("2026-01-01 00:00:00: int64", result.stdout)
        self.assertIn("Total rows across all sheets: 2", result.stdout)

    def test_single_row_date_header_is_strict_json(self) -> None:
        self.write_workbook([datetime(2026, 1, 1)], [[10]])
        result = self.run_reader("--json")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        report = json.loads(result.stdout, parse_constant=reject_json_constant)
        date_label = "2026-01-01 00:00:00"
        self.assertEqual(report["structure"]["Forecast"]["preview"], [{date_label: 10}])
        self.assertIsNone(report["stats"]["Forecast"][date_label]["std"])
        self.assertEqual(report["stats"]["Forecast"][date_label]["count"], 1.0)

    def test_date_header_nulls_are_strict_json(self) -> None:
        self.write_workbook(
            [datetime(2026, 1, 1), "Label"],
            [[None, "missing"], [30, "present"]],
        )
        result = self.run_reader("--json")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        report = json.loads(result.stdout, parse_constant=reject_json_constant)
        date_label = "2026-01-01 00:00:00"
        info = report["structure"]["Forecast"]
        self.assertEqual(info["null_columns"][date_label], {"count": 1, "pct": 50.0})
        self.assertIsNone(info["preview"][0][date_label])
        self.assertIsNone(report["stats"]["Forecast"][date_label]["std"])
        null_findings = [
            finding
            for finding in report["quality"]["Forecast"]
            if finding["type"] == "null_values"
        ]
        self.assertEqual(len(null_findings), 1)
        self.assertEqual(null_findings[0]["column"], date_label)

    def test_colliding_date_label_is_rejected_without_partial_json(self) -> None:
        self.write_workbook(
            [datetime(2026, 1, 1), "2026-01-01 00:00:00"],
            [[10, 20], [30, 40]],
        )
        result = self.run_reader("--json")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertIn("ERROR: JSON keys collide after conversion:", result.stderr)
        self.assertNotIn("Traceback", result.stderr)


if __name__ == "__main__":
    unittest.main()
