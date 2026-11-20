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

"""Summary statistics in `summarize_results.py` table mode.

Usage:
    pytest tests/test_summarize_results.py -v
"""

import json
import subprocess
import sys
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / "scripts" / "summarize_results.py"


def _summarize(tmp_path: Path, payload) -> subprocess.CompletedProcess:
    batch = tmp_path / "batch_results.json"
    batch.write_text(json.dumps(payload))
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--input", str(batch)],
        capture_output=True,
        text=True,
        check=False,
    )


def test_empty_batch_prints_a_zero_summary_instead_of_crashing(tmp_path):
    result = _summarize(tmp_path, [])

    assert result.returncode == 0, result.stderr
    assert "ZeroDivisionError" not in result.stderr
    assert "Summary: 0 tasks, 0 passed (0.0%)" in result.stdout
    assert "Trials: 0 total, 0 passed (0.0%)" in result.stdout


def test_summary_counts_passed_tasks_and_trials(tmp_path):
    payload = [
        {
            "task_id": "T001",
            "task_name": "first",
            "difficulty": "easy",
            "avg_passed": True,
            "trials": [{"passed": True}, {"passed": False}],
        },
        {
            "task_id": "T002",
            "task_name": "second",
            "difficulty": "easy",
            "avg_passed": False,
            "trials": [{"passed": False}],
        },
    ]

    result = _summarize(tmp_path, payload)

    assert result.returncode == 0, result.stderr
    assert "Summary: 2 tasks, 1 passed (50.0%)" in result.stdout
    assert "Trials: 3 total, 1 passed (33.3%)" in result.stdout
