# Copyright 2026 Hanabi21
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

"""Exercise complete report CLIs with valid batch artifacts and real file IO.

The analyze CLI uses its existing saved-report path: no judge or service is
replaced or contacted. Its real PyYAML/OpenAI imports must be available.
ANOLISA_REPORT_SCRIPTS selects an exact baseline/candidate scripts directory.
On Windows the baseline file writers insert an extra CR into each CSV record.
"""

import csv
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

PROJECT_SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"
SCRIPTS = Path(os.environ.get("ANOLISA_REPORT_SCRIPTS", PROJECT_SCRIPTS)).resolve()


class TestReportCsvNewlines(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="ce-report-csv-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.trace_dir = self.root / "traces" / "run_alpha"
        self.trace_dir.mkdir(parents=True)
        self.input_file = self.trace_dir / "batch_results.json"
        self.output_base = self.root / "reports_output"
        self.run_output = self.output_base / self.trace_dir.name
        trials = []
        for number, score in ((1, 1.0), (2, 0.5)):
            trials.append(
                {
                    "trial": number,
                    "trace_file": str(self.trace_dir / f"T001_trial{number}.jsonl"),
                    "task_score": score,
                    "passed": score >= 0.75,
                    "completion": score,
                    "robustness": score,
                    "communication": score,
                    "safety": 1.0,
                    "wall_time_s": 5.0,
                    "input_tokens": 17,
                    "output_tokens": 5,
                }
            )
        self.input_file.write_text(
            json.dumps(
                [
                    {
                        "task_id": "T001",
                        "task_name": 'Email, "calendar"',
                        "difficulty": "easy",
                        "trials": trials,
                        "error": None,
                        "avg_score": 0.75,
                        "pass_at_1": 0.5,
                        "pass_hat_k": 0.25,
                        "avg_passed": True,
                    }
                ]
            ),
            encoding="utf-8",
        )

    def run_cli(self, script_name: str, *arguments: str) -> subprocess.CompletedProcess[bytes]:
        env = os.environ.copy()
        env.update(
            {
                "PYTHONUTF8": "1",
                "PYTHONIOENCODING": "utf-8",
                "NO_PROXY": "127.0.0.1,localhost",
                "no_proxy": "127.0.0.1,localhost",
            }
        )
        result = subprocess.run(
            [sys.executable, "-X", "utf8=1", str(SCRIPTS / script_name), *arguments],
            cwd=self.root,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=20,
            check=False,
        )
        self.assertEqual(
            result.returncode,
            0,
            (result.stdout + result.stderr).decode("utf-8", errors="replace"),
        )
        return result

    def assert_csv_records(self, path: Path) -> None:
        raw = path.read_bytes()
        self.assertNotIn(b"\r\r\n", raw)
        with path.open(encoding="utf-8", newline="") as handle:
            rows = list(csv.reader(handle))
        # Keep blank records in the reader result: filtering them hides the bug.
        self.assertEqual(len(rows), 3)
        self.assertTrue(all(rows))
        self.assertEqual(rows[0][:3], ["Task ID", "Task Name", "Difficulty"])
        self.assertEqual(rows[1][:3], ["T001", 'Email, "calendar"', "easy"])
        self.assertEqual(rows[1][3], "#1")
        self.assertEqual(rows[2][3], "#2")
        score_column = rows[0].index("Task Score")
        self.assertEqual(rows[1][score_column], "1")
        self.assertEqual(rows[2][score_column], "0.50")

    def run_cached_analyze(self) -> subprocess.CompletedProcess[bytes]:
        report_dir = self.run_output / "reports"
        report_dir.mkdir(parents=True)
        for number, passed in ((1, True), (2, False)):
            (report_dir / f"T001_trial{number}.json").write_text(
                json.dumps(
                    {
                        "trace_file": f"T001_trial{number}.jsonl",
                        "task_id": "T001",
                        "task_name": 'Email, "calendar"',
                        "trial_id": f"trial{number}",
                        "status": "succ" if passed else "fail",
                        "task_score": 1.0 if passed else 0.5,
                        "scores": {},
                    }
                ),
                encoding="utf-8",
            )
        result = self.run_cli(
            "analyze.py",
            "--trace-dir",
            str(self.trace_dir),
            "--output-dir",
            str(self.output_base),
            "--judge-api-key",
            "unused-cached-reports",
            "--judge-base-url",
            "http://127.0.0.1:1/v1",
            "--judge-model",
            "unused-cached-reports",
        )
        self.assertIn(b"existing reports, skipping generation", result.stderr)
        return result

    def test_summarize_csv_file_has_one_record_per_rendered_row(self) -> None:
        output = self.root / "summary.csv"
        self.run_cli(
            "summarize_results.py",
            "--input",
            str(self.input_file),
            "--format",
            "csv",
            "--output",
            str(output),
        )
        self.assert_csv_records(output)

    def test_analyze_csv_file_has_one_record_per_rendered_row(self) -> None:
        self.run_cached_analyze()
        self.assert_csv_records(self.run_output / "summary.csv")

    def test_summarize_table_output_control(self) -> None:
        output = self.root / "summary.txt"
        self.run_cli(
            "summarize_results.py",
            "--input",
            str(self.input_file),
            "--format",
            "table",
            "--output",
            str(output),
        )
        text = output.read_text(encoding="utf-8")
        self.assertIn('Email, "calendar"', text)
        self.assertIn("Summary: 1 tasks", text)
        self.assertIn("Trials: 2 total, 1 passed (50.0%)", text)

    def test_analyze_text_and_structured_output_control(self) -> None:
        self.run_cached_analyze()
        text = (self.run_output / "summary.txt").read_text(encoding="utf-8")
        self.assertIn("Trials: 2 total, 1 passed (50.0%)", text)
        report = json.loads((self.run_output / "report.json").read_text(encoding="utf-8"))
        self.assertEqual(report["trace_name"], "run_alpha")
        self.assertEqual(report["summary"]["total_tasks"], 1)
        self.assertEqual(report["summary"]["total_trials"], 2)
        self.assertEqual(report["tasks"][0]["task_name"], 'Email, "calendar"')
        self.assertEqual(len(report["tasks"][0]["trials"]), 2)


if __name__ == "__main__":
    unittest.main()
