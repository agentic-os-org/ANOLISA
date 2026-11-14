# Copyright 2026 project-nju-learner
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Offline regressions for preserving diagnostics in report output."""

import csv
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import ModuleType

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"
TRACE = "T001_example_trial-one.jsonl"
REASON = 'Expected {"x":1,"y":2}, received "empty"，请重试\nsecond line'
DATA = [
    {
        "task_id": "T001",
        "task_name": "Example",
        "difficulty": "easy",
        "avg_passed": False,
        "trials": [{"trace": TRACE, "passed": False}],
    }
]


def load_script(name: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def failure_report() -> dict:
    return {
        "trace_file": TRACE,
        "status": "fail",
        "failure_classification": {"category": "tool_error", "key_reason_zh": REASON},
    }


class ReportFailureTextTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.summarize = load_script("summarize_results")
        cls.analyze = load_script("analyze")

    def test_both_csv_reports_preserve_classified_reason(self) -> None:
        for module, builder in (
            (self.summarize, self.summarize.build_table),
            (self.analyze, self.analyze.build_summary_table),
        ):
            with self.subTest(script=module.__name__):
                rows = builder(DATA, {TRACE: failure_report()})
                parsed = list(csv.reader(io.StringIO(module.render_csv(rows))))
                self.assertEqual(len(parsed), 2)
                self.assertEqual(len(parsed[1]), len(parsed[0]))
                index = parsed[0].index("Failure Reason")
                self.assertEqual(parsed[1][index], f"tool_error | {REASON}")

    def test_both_extractors_preserve_fallback_reason(self) -> None:
        report = {"status": "fail", "failure_reason": [{"reasoning": REASON}]}
        for module in (self.summarize, self.analyze):
            with self.subTest(script=module.__name__):
                self.assertEqual(module.extract_failure_info(report), REASON)

    def test_structured_json_preserves_classified_reason(self) -> None:
        result = self.analyze.build_structured_data(DATA, {TRACE: failure_report()}, "example")
        result = json.loads(json.dumps(result))
        self.assertEqual(result["tasks"][0]["trials"][0]["failure_reason"], REASON)
        self.assertEqual(result["failure_distribution"], {"tool_error": 1})

    def test_text_reports_preserve_punctuation(self) -> None:
        for module, builder in (
            (self.summarize, self.summarize.build_table),
            (self.analyze, self.analyze.build_summary_table),
        ):
            with self.subTest(script=module.__name__):
                rows = builder(DATA, {TRACE: failure_report()})
                self.assertIn(REASON, module.render_table(rows))

    def test_missing_and_successful_reports_keep_sentinels(self) -> None:
        for module in (self.summarize, self.analyze):
            with self.subTest(script=module.__name__):
                self.assertEqual(module.extract_failure_info(None), "N/A")
                self.assertEqual(module.extract_failure_info({"status": "succ"}), "-")
                self.assertEqual(module.extract_failure_info({"status": "fail"}), "unknown")

    def test_fallback_keeps_existing_summary_limits(self) -> None:
        first = "x" * 100
        report = {
            "status": "fail",
            "failure_reason": [
                {"reasoning": first},
                {"message": "second"},
                {"reasoning": "third"},
            ],
        }
        for module in (self.summarize, self.analyze):
            with self.subTest(script=module.__name__):
                self.assertEqual(module.extract_failure_info(report), first[:80] + "; second")

    def test_summary_cli_csv_round_trip(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            batch = work / "batch_results.json"
            reports = work / "reports"
            reports.mkdir()
            batch.write_text(json.dumps(DATA), encoding="utf-8")
            (reports / "trial.json").write_text(json.dumps(failure_report()), encoding="utf-8")
            environment = dict(os.environ, PYTHONUTF8="1")
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPTS / "summarize_results.py"),
                    "--input",
                    str(batch),
                    "--report-dir",
                    str(reports),
                    "--format",
                    "csv",
                ],
                capture_output=True,
                text=True,
                encoding="utf-8",
                env=environment,
                check=False,
                timeout=30,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            rows = list(csv.DictReader(io.StringIO(result.stdout)))
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]["Failure Reason"], f"tool_error | {REASON}")


if __name__ == "__main__":
    unittest.main(verbosity=2)
