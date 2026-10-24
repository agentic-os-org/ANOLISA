"""Subprocess coverage of offline difficulty-level batch summaries."""

from __future__ import annotations

import csv
import io
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "summarize_results.py"
HEADERS = [
    "Difficulty",
    "Tasks",
    "Trials",
    "Error Trials",
    "Evaluated Trials",
    "Passed Trials",
    "Pass Rate",
    "Scored Trials",
    "Mean Score",
    "Timed Trials",
    "Mean Wall(s)",
]


def invoke(tmp_path: Path, data: object, *flags: str, encoding: str | None = None):
    source = tmp_path / "batch_results.json"
    source.write_text(json.dumps(data, ensure_ascii=False), encoding="utf-8")
    environment = dict(os.environ)
    if encoding:
        environment["PYTHONIOENCODING"] = encoding
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--input",
            str(source),
            "--group-by",
            "difficulty",
            *flags,
        ],
        env=environment,
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=10,
        check=False,
    )
    return result


def rows(output: str) -> list[dict[str, str]]:
    reader = csv.DictReader(io.StringIO(output))
    assert reader.fieldnames == HEADERS
    return list(reader)


def test_group_summary_weights_trials_and_counts_denominators(tmp_path):
    data = [
        {
            "difficulty": "hard",
            "trials": [
                {"passed": True, "task_score": 1, "wall_time_s": 2},
                {"passed": False, "task_score": 0, "wall_time_s": 6},
                {
                    "passed": False,
                    "task_score": 0,
                    "wall_time_s": 500,
                    "error": "fixture failed",
                },
            ],
        },
        {
            "difficulty": "hard",
            "trials": [{"passed": True, "task_score": 1, "wall_time_s": 4}],
        },
        {
            "difficulty": "easy",
            "trials": [{"passed": False, "task_score": 0.25, "wall_time_s": 1}],
        },
    ]
    result = invoke(tmp_path, data, "--format", "csv")
    assert result.returncode == 0, result.stderr
    found = rows(result.stdout)
    assert [row["Difficulty"] for row in found] == ["easy", "hard"]
    hard = found[1]
    assert {key: hard[key] for key in HEADERS[:6]} == {
        "Difficulty": "hard",
        "Tasks": "2",
        "Trials": "4",
        "Error Trials": "1",
        "Evaluated Trials": "3",
        "Passed Trials": "2",
    }
    assert float(hard["Pass Rate"]) == pytest.approx(2 / 3, abs=1e-4)
    assert hard["Scored Trials"] == hard["Timed Trials"] == "3"
    assert float(hard["Mean Score"]) == pytest.approx(2 / 3, abs=1e-4)
    assert float(hard["Mean Wall(s)"]) == 4


def test_missing_measurements_have_separate_denominators_and_blank_means(tmp_path):
    result = invoke(
        tmp_path,
        [
            {
                "difficulty": "easy",
                "trials": [
                    {"passed": True, "task_score": None, "wall_time_s": 0},
                    {"passed": None, "task_score": 0.5},
                ],
            },
            {"difficulty": "hard", "trials": [{"error": "no evidence"}]},
            {"difficulty": "empty", "trials": []},
        ],
        "--format",
        "csv",
    )
    assert result.returncode == 0, result.stderr
    found = {row["Difficulty"]: row for row in rows(result.stdout)}
    assert (
        found["easy"]["Evaluated Trials"]
        == found["easy"]["Scored Trials"]
        == found["easy"]["Timed Trials"]
        == "1"
    )
    assert found["easy"]["Pass Rate"] == "1"
    assert found["easy"]["Mean Score"] == "0.5"
    assert found["easy"]["Mean Wall(s)"] == "0"
    for key in ("Pass Rate", "Mean Score", "Mean Wall(s)"):
        assert found["hard"][key] == found["empty"][key] == ""


def test_unicode_difficulty_csv_survives_ascii_stdout_and_file_output(tmp_path):
    label = '复杂, "emoji" 😀'
    data = [
        {"difficulty": label, "trials": []},
        {"trials": []},
        {"difficulty": "", "trials": []},
    ]
    result = invoke(tmp_path, data, "--format", "csv", encoding="ascii")
    assert result.returncode == 0, result.stderr
    found = rows(result.stdout)
    assert {row["Difficulty"] for row in found} == {label, "unknown"}
    assert next(row for row in found if row["Difficulty"] == "unknown")["Tasks"] == "2"
    target = tmp_path / "grouped.csv"
    result = invoke(
        tmp_path, data, "--format", "csv", "--output", str(target), encoding="ascii"
    )
    assert result.returncode == 0, result.stderr
    assert rows(target.read_text(encoding="utf-8")) == found


def test_invalid_measurements_are_unavailable_and_large_mean_is_finite(tmp_path):
    result = invoke(
        tmp_path,
        [
            {
                "difficulty": "hard",
                "trials": [
                    {
                        "passed": 1,
                        "task_score": float("nan"),
                        "wall_time_s": float("inf"),
                    },
                    {"passed": "false", "task_score": True, "wall_time_s": -1},
                    {"task_score": 1, "wall_time_s": 1e308},
                    {"task_score": 0, "wall_time_s": 1e308},
                    {"task_score": -1, "wall_time_s": 10**400},
                ],
            }
        ],
        "--format",
        "csv",
    )
    assert result.returncode == 0, result.stderr
    (row,) = rows(result.stdout)
    assert row["Evaluated Trials"] == "0" and row["Pass Rate"] == ""
    assert row["Scored Trials"] == row["Timed Trials"] == "2"
    assert float(row["Mean Score"]) == 0.5
    assert float(row["Mean Wall(s)"]) == 1e308
    assert "nan" not in result.stdout.lower() and "inf" not in result.stdout.lower()


@pytest.mark.parametrize(
    "data",
    [
        None,
        {},
        [None],
        [{"trials": {}}],
        [{"trials": [None]}],
        [{"difficulty": [], "trials": []}],
    ],
)
def test_malformed_group_source_reports_error_before_output(tmp_path, data):
    target = tmp_path / "existing.csv"
    target.write_text("previous export\n", encoding="utf-8")
    result = invoke(tmp_path, data, "--format", "csv", "--output", str(target))
    assert result.returncode == 1, result.stderr
    assert "Error:" in result.stderr
    assert target.read_text(encoding="utf-8") == "previous export\n"


@pytest.mark.parametrize("format_name", ["csv", "table"])
def test_empty_grouped_source_is_supported(tmp_path, format_name):
    result = invoke(tmp_path, [], "--format", format_name)
    assert result.returncode == 0, result.stderr
    assert "Difficulty" in result.stdout and "Mean Score" in result.stdout
    if format_name == "csv":
        assert rows(result.stdout) == []


def test_default_task_table_preserves_existing_layout(tmp_path):
    source = tmp_path / "batch_results.json"
    source.write_text(
        json.dumps(
            [
                {
                    "task_id": "case",
                    "task_name": "fixture",
                    "difficulty": "easy",
                    "trials": [{"passed": True, "task_score": 1}],
                    "avg_score": 1,
                }
            ]
        ),
        encoding="utf-8",
    )
    result = subprocess.run(
        [sys.executable, str(SCRIPT), "--input", str(source), "--format", "csv"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
        timeout=10,
    )
    assert result.returncode == 0, result.stderr
    reader = csv.reader(io.StringIO(result.stdout))
    header, row = [row for row in reader if row]
    assert header[:3] == ["Task ID", "Task Name", "Difficulty"]
    assert row[:4] == ["case", "fixture", "easy", "#1"]
