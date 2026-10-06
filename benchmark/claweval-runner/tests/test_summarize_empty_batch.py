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

"""Empty batch_results.json must summarize without a ZeroDivisionError.

Both summarize_results.py and analyze.py divide by total_tasks /
total_trials when rendering the human-readable summary footer; an empty
batch (a crashed run that still wrote a valid empty list) crashed the
summary step. build_structured_data already guards these divisions.
"""

import json
import subprocess
import sys
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"
SUMMARIZE = SCRIPTS / "summarize_results.py"
ANALYZE = SCRIPTS / "analyze.py"


def test_summarize_results_empty_batch(tmp_path):
    input_file = tmp_path / "batch_results.json"
    input_file.write_text(json.dumps([]), encoding="utf-8")

    proc = subprocess.run(
        [sys.executable, str(SUMMARIZE), "--input", str(input_file), "--format", "table"],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stderr
    assert "0 tasks, 0 passed (0.0%)" in proc.stdout
    assert "0 total, 0 passed (0.0%)" in proc.stdout


def test_summarize_results_no_trials(tmp_path):
    input_file = tmp_path / "batch_results.json"
    input_file.write_text(
        json.dumps(
            [
                {
                    "task_id": "T001",
                    "task_name": "solo",
                    "difficulty": "easy",
                    "trials": [],
                }
            ]
        ),
        encoding="utf-8",
    )

    proc = subprocess.run(
        [sys.executable, str(SUMMARIZE), "--input", str(input_file), "--format", "table"],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stderr
    assert "1 tasks, 0 passed (0.0%)" in proc.stdout
    assert "0 total, 0 passed (0.0%)" in proc.stdout


def test_analyze_empty_batch(tmp_path):
    trace_dir = tmp_path / "traces" / "empty_run"
    trace_dir.mkdir(parents=True)
    (trace_dir / "batch_results.json").write_text(json.dumps([]), encoding="utf-8")
    out_dir = tmp_path / "results"

    proc = subprocess.run(
        [
            sys.executable,
            str(ANALYZE),
            "--trace-dir",
            str(trace_dir),
            "--tasks-dir",
            str(tmp_path / "tasks"),
            "--output-dir",
            str(out_dir),
            "--judge-model",
            "test-judge",
            "--judge-api-key",
            "sk-test",
        ],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stderr
    summary = (out_dir / "empty_run" / "summary.txt").read_text(encoding="utf-8")
    assert "0 tasks, 0 passed (0.0%)" in summary
    assert "0 total, 0 passed (0.0%)" in summary
