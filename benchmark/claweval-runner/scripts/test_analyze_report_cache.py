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

"""Regression test: analyze.py report cache must key on the current trace.

The resume step used to scan ALL run dirs under the output base and reuse
the newest one holding any reports. Reports are keyed by trace file name
(trial hash), so analyzing a NEW trace dir silently reused an OLD run's
reports: generation skipped, failure attribution null, exit 0.

Resume must only consult <output_base>/<trace_name>/reports.
"""

import importlib.util
import json
import os
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


def _fake_generate_reports(calls):
    """Stand-in for the LLM report generator: writes one report per trace."""

    def _generate(trace_dir, tasks_dir, report_dir, judge_api_key,
                  judge_base_url, judge_model_id):
        calls.append(report_dir)
        os.makedirs(report_dir, exist_ok=True)
        for tf in sorted(Path(trace_dir).glob("*.jsonl")):
            report = {
                "trace_file": tf.name,
                "status": "fail",
                "failure_classification": {
                    "category": "tool_error",
                    "key_reason_zh": f"generated for {tf.name}",
                },
            }
            (Path(report_dir) / (tf.stem + ".json")).write_text(
                json.dumps(report), encoding="utf-8")
        return 1

    return _generate


class AnalyzeReportCacheTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        base = Path(self._tmp.name)
        self.output_base = base / "results"
        self.trace_dir = base / "traceB"
        self.trace_dir.mkdir(parents=True)

        # Old run's report for a DIFFERENT trace (different trial hash).
        old_reports = self.output_base / "prevtrace" / "reports"
        old_reports.mkdir(parents=True)
        (old_reports / "T001_aaaa1111.json").write_text(json.dumps({
            "trace_file": "T001_aaaa1111.jsonl", "status": "fail",
            "failure_classification": {"category": "tool_error",
                                       "key_reason_zh": "old run's reason"},
        }), encoding="utf-8")

        # New trace dir: failing trial with a different hash.
        (self.trace_dir / "T001_bbbb2222.jsonl").write_text("\n".join([
            json.dumps({"type": "trace_start", "task_id": "T001"}),
            json.dumps({"type": "grading_result", "task_id": "T001",
                        "passed": False, "task_score": 0.2,
                        "scores": {"completion": 0.2}}),
            json.dumps({"type": "trace_end", "task_id": "T001"}),
        ]) + "\n", encoding="utf-8")
        (self.trace_dir / "batch_results.json").write_text(json.dumps([{
            "task_id": "T001", "task_name": "t", "difficulty": "easy",
            "trials": [{
                "trial": 1, "task_score": 0.2, "passed": False,
                "completion": 0.2, "robustness": 0.2, "communication": 0.2,
                "safety": 1.0, "error": None, "wall_time_s": 10.0,
                "trace": "T001_bbbb2222.jsonl",
            }],
            "error": None, "avg_score": 0.2, "pass_at_1": 0.0,
            "pass_hat_k": 0.0, "avg_passed": False,
        }]), encoding="utf-8")

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

    def _run_main(self, mod, calls):
        with mock.patch.object(mod, "generate_reports",
                               side_effect=_fake_generate_reports(calls)), \
             mock.patch.object(sys, "argv", self._argv):
            mod.main()

    def test_new_trace_generates_reports_despite_old_run(self):
        """A new trace must not reuse an older run's reports."""
        mod = _load_analyze()
        calls = []
        self._run_main(mod, calls)
        self.assertEqual(len(calls), 1, "generation must run for a new trace")
        self.assertEqual(Path(calls[0]).name, "reports")
        self.assertEqual(Path(calls[0]).parent.name, "traceB")

        structured = json.loads(
            (self.output_base / "traceB" / "report.json").read_text())
        trial = structured["tasks"][0]["trials"][0]
        self.assertEqual(trial["failure_category"], "tool_error")
        self.assertEqual(trial["failure_reason"],
                         "generated for T001_bbbb2222.jsonl")

    def test_same_trace_resume_skips_generation(self):
        """Re-running the SAME trace dir still resumes from its reports."""
        mod = _load_analyze()
        calls = []
        self._run_main(mod, calls)   # first run generates
        self.assertEqual(len(calls), 1)
        self._run_main(mod, calls)   # second run must reuse, not regenerate
        self.assertEqual(len(calls), 1, "same-trace resume must skip generation")

        structured = json.loads(
            (self.output_base / "traceB" / "report.json").read_text())
        trial = structured["tasks"][0]["trials"][0]
        self.assertEqual(trial["failure_category"], "tool_error")


if __name__ == "__main__":
    unittest.main()
