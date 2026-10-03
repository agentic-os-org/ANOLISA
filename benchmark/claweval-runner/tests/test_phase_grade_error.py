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

"""Regression test: phase_grade must not swallow grader crashes.

When the grader subprocess exits rc != 0 without ever appending a
grading_result event to the trace, the returned dict must carry an
"error" key so batch_runner counts the trial as errored (excluded from
avg_score / pass^k) instead of a genuine 0.0 FAIL.
"""

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))

from ce_runner import pipeline  # noqa: E402

JUDGE_CONFIG = {"model": "m", "base_url": "http://x", "api_key": "k"}


def _write_trace(trace_file: str, events: list) -> None:
    with open(trace_file, "w") as f:
        for event in events:
            f.write(json.dumps(event) + "\n")


class _FakeCompletedProcess:
    def __init__(self, returncode: int):
        self.returncode = returncode


class PhaseGradeGraderCrashTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.trace_file = os.path.join(self._tmp.name, "T001_deadbeef.jsonl")

    def tearDown(self):
        self._tmp.cleanup()

    def _phase_grade_with_rc(self, rc: int) -> dict:
        with mock.patch.object(
            pipeline.subprocess, "run", return_value=_FakeCompletedProcess(rc)
        ) as run:
            result = pipeline.phase_grade(
                self.trace_file, "/nonexistent/task.yaml", JUDGE_CONFIG
            )
        run.assert_called_once()  # grader subprocess was (stub)launched
        return result

    def test_crash_without_grading_result_marks_error(self):
        """rc != 0 and no grading_result event -> error key, zero scores."""
        _write_trace(self.trace_file, [
            {"type": "trace_start", "task_id": "T001"},
            {"type": "trace_end", "task_id": "T001"},
        ])
        result = self._phase_grade_with_rc(1)
        self.assertEqual(result["error"], "grader_rc_1")
        self.assertEqual(result["task_score"], 0.0)
        self.assertFalse(result["passed"])
        # batch_runner's validity check: an errored trial is excluded.
        self.assertTrue(result.get("error"))

    def test_crash_after_grading_keeps_scores(self):
        """rc != 0 but grading_result landed -> scores stay authoritative."""
        _write_trace(self.trace_file, [
            {"type": "trace_start", "task_id": "T001"},
            {"type": "grading_result", "task_id": "T001", "passed": True,
             "task_score": 0.9, "scores": {"completion": 0.9}},
        ])
        result = self._phase_grade_with_rc(2)
        self.assertNotIn("error", result)
        self.assertTrue(result["passed"])
        self.assertEqual(result["task_score"], 0.9)

    def test_clean_run_has_no_error(self):
        """rc == 0 with grading_result -> unchanged behaviour, no error."""
        _write_trace(self.trace_file, [
            {"type": "trace_start", "task_id": "T001"},
            {"type": "grading_result", "task_id": "T001", "passed": False,
             "task_score": 0.2, "scores": {"completion": 0.2}},
        ])
        result = self._phase_grade_with_rc(0)
        self.assertNotIn("error", result)
        self.assertEqual(result["task_score"], 0.2)


if __name__ == "__main__":
    unittest.main()
