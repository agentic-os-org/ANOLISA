#!/usr/bin/env python3

# Copyright 2026 Alibaba Cloud
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

"""Regression test: analyze.py main() must survive empty batch_results.json.

main()'s summary text divided by total_tasks/total_trials unguarded, so an
empty batch_results.json crashed with ZeroDivisionError AFTER the LLM
report generation phase, leaving no summary/report files at all. Same
guard build_structured_data already applies to its ratios.
"""

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).resolve().parent / "analyze.py"


def _load_analyze():
    spec = importlib.util.spec_from_file_location("analyze_under_test", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _fake_generate_reports(trace_dir, tasks_dir, report_dir, judge_api_key,
                           judge_base_url, judge_model_id):
    """Stand-in so no LLM is called; emulates the generation phase having run."""
    os_mkdir = __import__("os").makedirs
    os_mkdir(report_dir, exist_ok=True)
    return 0


class AnalyzeEmptySummaryTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        base = Path(self._tmp.name)
        self.trace_dir = base / "traces"
        self.output_base = base / "results"
        self.trace_dir.mkdir(parents=True)
        (self.trace_dir / "T001_ok77.jsonl").write_text(json.dumps({
            "type": "grading_result", "task_id": "T001", "passed": True,
            "task_score": 1.0, "scores": {"completion": 1.0},
        }) + "\n", encoding="utf-8")
        self._argv = [
            "analyze.py",
            "--trace-dir", str(self.trace_dir),
            "--output-dir", str(self.output_base),
            "--tasks-dir", "/nonexistent",
            "--judge-api-key", "sk-test",
            "--judge-model", "m1",
        ]

    def tearDown(self):
        self._tmp.cleanup()

    def _run_main(self, batch_results):
        (self.trace_dir / "batch_results.json").write_text(
            json.dumps(batch_results), encoding="utf-8")
        mod = _load_analyze()
        with mock.patch.object(mod, "generate_reports",
                               side_effect=_fake_generate_reports), \
             mock.patch.object(sys, "argv", self._argv):
            mod.main()  # must not raise

    def test_empty_batch_results_writes_outputs(self):
        """[] must yield a 0-task summary and all three output files."""
        self._run_main([])
        out = self.output_base / self.trace_dir.name
        summary = (out / "summary.txt").read_text(encoding="utf-8")
        self.assertIn("Summary: 0 tasks, 0 passed (0.0%)", summary)
        self.assertIn("Trials: 0 total, 0 passed (0.0%)", summary)
        self.assertTrue((out / "summary.csv").exists())
        structured = json.loads((out / "report.json").read_text(encoding="utf-8"))
        self.assertEqual(structured["summary"]["total_tasks"], 0)
        self.assertEqual(structured["summary"]["pass_rate"], 0)

    def test_non_empty_summary_percentages_unchanged(self):
        """Normal input keeps its percentages (guard is a no-op here)."""
        self._run_main([{
            "task_id": "T001", "task_name": "t", "difficulty": "easy",
            "trials": [{
                "trial": 1, "task_score": 1.0, "passed": True,
                "completion": 1.0, "robustness": 1.0, "communication": 1.0,
                "safety": 1.0, "error": None, "wall_time_s": 1.0,
                "trace": "T001_ok77.jsonl",
            }],
            "error": None, "avg_score": 1.0, "pass_at_1": 1.0,
            "pass_hat_k": 1.0, "avg_passed": True,
        }])
        summary = (
            self.output_base / self.trace_dir.name / "summary.txt"
        ).read_text(encoding="utf-8")
        self.assertIn("Summary: 1 tasks, 1 passed (100.0%)", summary)
        self.assertIn("Trials: 1 total, 1 passed (100.0%)", summary)


if __name__ == "__main__":
    unittest.main()
