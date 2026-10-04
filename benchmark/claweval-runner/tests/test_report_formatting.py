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

import importlib.util
import json
import sys
from pathlib import Path
from types import ModuleType

import pytest

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"


@pytest.fixture(params=["analyze", "summarize_results"])
def entrypoint(
    request: pytest.FixtureRequest, monkeypatch: pytest.MonkeyPatch
) -> ModuleType:
    monkeypatch.syspath_prepend(str(SCRIPTS))
    spec = importlib.util.spec_from_file_location(
        f"report_format_{request.param}", SCRIPTS / f"{request.param}.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.mark.parametrize("with_reports", [False, True])
def test_trial_rows_keep_order_and_summary_cells(
    entrypoint: ModuleType, with_reports: bool
) -> None:
    data = [
        {
            "task_id": "T002",
            "task_name": "No trials",
            "difficulty": "hard",
            "trials": [],
        },
        {
            "task_id": "T001",
            "task_name": "邮件,整理",
            "difficulty": "easy",
            "avg_score": 0.75,
            "pass_at_1": 0.5,
            "pass_hat_k": 0,
            "avg_passed": True,
            "trials": [
                {
                    "trace": "traces/T001_abc123.jsonl",
                    "input_tokens": 10,
                    "output_tokens": 2.0,
                    "model_time_s": 1.125,
                    "completion": 1.0,
                    "task_score": 0.5,
                    "passed": False,
                },
                {"trace": "T001_def456.jsonl", "input_tokens": 20, "passed": True},
            ],
        },
    ]
    reports = (
        {
            "T001_abc123.jsonl": {
                "failure_classification": {
                    "category": "tool_error",
                    "key_reason_zh": "一,二，三",
                }
            }
        }
        if with_reports
        else {}
    )
    builder = getattr(entrypoint, "build_summary_table", None) or entrypoint.build_table
    rows = builder(data, reports)
    expected = [
        [
            "T001",
            "邮件,整理",
            "easy",
            "#1",
            "abc123",
            "10",
            "2",
            "1.12",
            "N/A",
            "N/A",
            "N/A",
            "1",
            "N/A",
            "N/A",
            "N/A",
            "0.50",
            "N",
            "0.75",
            "0.50",
            "0",
            "Y",
        ],
        [
            "",
            "",
            "",
            "#2",
            "def456",
            "20",
            "N/A",
            "N/A",
            "N/A",
            "N/A",
            "N/A",
            "N/A",
            "N/A",
            "N/A",
            "N/A",
            "N/A",
            "Y",
            "",
            "",
            "",
            "",
        ],
    ]
    if with_reports:
        expected[0].append("tool_error | 一;二；三")
        expected[1].append("N/A")
    assert rows[1:] == expected
    assert len(rows[0]) == 22 if with_reports else len(rows[0]) == 21
    assert rows[0][:5] == ["Task ID", "Task Name", "Difficulty", "Trial", "Trial ID"]
    assert entrypoint.render_csv(rows).startswith(
        "Task ID,Task Name,Difficulty,Trial,Trial ID,"
    )


def test_table_rendering_matches_existing_bytes(entrypoint: ModuleType) -> None:
    rows = [["task", "score"], ["T001", "0.25"], ["T002", "N/A"]]
    expected = (
        "+------+-------+\n| task | score |\n+------+-------+\n"
        "| T001 | 0.25  |\n+------+-------+\n| T002 | N/A   |\n+------+-------+"
    )
    assert entrypoint.render_table(rows) == expected


def test_csv_rendering_preserves_quoting_and_newlines(entrypoint: ModuleType) -> None:
    rows = [["Task", "Reason"], ["T001", 'comma, quote" and\n新行']]
    assert (
        entrypoint.render_csv(rows)
        == 'Task,Reason\r\nT001,"comma, quote"" and\n新行"\r\n'
    )


@pytest.mark.parametrize(
    "report, expected",
    [
        ({}, "N/A"),
        ({"status": "succ"}, "-"),
        (
            {
                "failure_reason": [
                    {"reasoning": "one"},
                    {"message": "two"},
                    {"message": "three"},
                ]
            },
            "one; two",
        ),
        ({"failure_reason": ["unstructured"]}, "unknown"),
    ],
)
def test_failure_fallbacks_remain_stable(
    entrypoint: ModuleType, report: dict, expected: str
) -> None:
    assert entrypoint.extract_failure_info(report) == expected


def test_offline_report_cli_uses_shared_formats(
    entrypoint: ModuleType, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    trace_dir = tmp_path / "traces" / "batch"
    trace_dir.mkdir(parents=True)
    task = {
        "task_id": "T001",
        "task_name": "邮件整理",
        "difficulty": "easy",
        "avg_score": 0.75,
        "avg_passed": True,
        "trials": [{"trace": "T001_abc123.jsonl", "task_score": 0.75, "passed": True}],
    }
    input_file = trace_dir / "batch_results.json"
    input_file.write_text(json.dumps([task]), encoding="utf-8")
    output = tmp_path / "results"
    if hasattr(entrypoint, "build_summary_table"):
        monkeypatch.setattr(
            entrypoint,
            "load_config",
            lambda *args: {
                "trace_dir": str(trace_dir),
                "tasks_dir": str(tmp_path),
                "judge_api_key": "offline",
                "judge_model_id": "offline-model",
                "judge_base_url": "",
            },
        )
        monkeypatch.setattr(entrypoint, "generate_reports", lambda **kwargs: None)
        monkeypatch.setattr(
            sys,
            "argv",
            ["analyze.py", "--trace-dir", str(trace_dir), "--output-dir", str(output)],
        )
        entrypoint.main()
        csv_path = output / "batch" / "summary.csv"
        assert (output / "batch" / "summary.txt").is_file()
        assert (output / "batch" / "report.json").is_file()
    else:
        csv_path = tmp_path / "summary.csv"
        monkeypatch.setattr(
            sys,
            "argv",
            [
                "summarize_results.py",
                "--input",
                str(input_file),
                "--format",
                "csv",
                "--output",
                str(csv_path),
            ],
        )
        entrypoint.main()
    assert "邮件整理" in csv_path.read_text(encoding="utf-8")
    assert "abc123" in csv_path.read_text(encoding="utf-8")
