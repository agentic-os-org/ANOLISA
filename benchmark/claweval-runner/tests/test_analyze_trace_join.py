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

"""Report joins must work for ce-runner batch output.

``ce_runner.batch_runner`` writes trial entries with a ``trace_file`` key
(while native claw-eval writes ``trace``); ``generate_trial_reports.py``
keys the per-trial reports by the trace-file basename. The analysis
scripts must join on whichever key the trial carries.
"""

import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO_ROOT / "scripts"))

TRACE_NAME = "T001zh_email_triage_8a61ea06.jsonl"


def _batch_task(trace_key: str) -> list:
    return [{
        "task_id": "T001zh_email_triage",
        "task_name": "Email Triage",
        "difficulty": "easy",
        "avg_score": 0.32,
        "pass_at_1": 0.0,
        "pass_hat_k": 0.0,
        "avg_passed": False,
        "trials": [{
            "trial": 1,
            "task_score": 0.32,
            "passed": False,
            "completion": 0.3,
            "robustness": 0.4,
            "communication": 0.2,
            "safety": 1.0,
            "error": None,
            "wall_time_s": 12.5,
            trace_key: TRACE_NAME,
        }],
    }]


def _reports() -> dict:
    return {
        TRACE_NAME: {
            "status": "fail",
            "failure_classification": {
                "category": "incorrect_execution",
                "key_reason_zh": "missed the important flag",
            },
        },
    }


class TestAnalyzeSummaryTable:
    def test_trace_file_trials_join_their_reports(self):
        import analyze

        rows = analyze.build_summary_table(_batch_task("trace_file"), _reports())
        header, row = rows[0], rows[1]
        assert row[header.index("Trial ID")] == "8a61ea06"
        reason = row[header.index("Failure Reason")]
        assert "incorrect_execution" in reason
        assert "missed the important flag" in reason

    def test_native_trace_key_still_joins(self):
        import analyze

        rows = analyze.build_summary_table(_batch_task("trace"), _reports())
        header, row = rows[0], rows[1]
        assert row[header.index("Trial ID")] == "8a61ea06"
        assert "incorrect_execution" in row[header.index("Failure Reason")]


class TestSummarizeResultsTable:
    def test_trace_file_trials_join_their_reports(self):
        import summarize_results

        rows = summarize_results.build_table(_batch_task("trace_file"), _reports())
        header, row = rows[0], rows[1]
        assert row[header.index("Trial ID")] == "8a61ea06"
        reason = row[header.index("Failure Reason")]
        assert "incorrect_execution" in reason
        assert "missed the important flag" in reason

    def test_native_trace_key_still_joins(self):
        import summarize_results

        rows = summarize_results.build_table(_batch_task("trace"), _reports())
        header, row = rows[0], rows[1]
        assert row[header.index("Trial ID")] == "8a61ea06"
        assert "incorrect_execution" in row[header.index("Failure Reason")]
