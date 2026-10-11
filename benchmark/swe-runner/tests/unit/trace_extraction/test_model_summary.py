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

import csv
import json
from pathlib import Path
from typing import Any

import pytest

from swe_runner.trace_extraction import export


def _trace(root: Path, instance: str, number: int, **values: Any) -> None:
    directory = root / instance
    directory.mkdir(parents=True, exist_ok=True)
    (directory / f"trace{number}.json").write_text(json.dumps(values), encoding="utf-8")


def _model_rows(root: Path, output: Path) -> list[dict[str, str]]:
    export.write_trace_analysis_csvs(root, output)
    with (output / "trace_model_summary.csv").open(encoding="utf-8", newline="") as source:
        return list(csv.DictReader(source))


def test_models_are_separated_within_a_case_and_combined_across_cases(tmp_path: Path) -> None:
    root = tmp_path / "traces"
    _trace(root, "case-1", 1, model="model-a", total_input_tokens=10, total_output_tokens=5, total_steps=1)
    _trace(root, "case-1", 2, model="model-b", total_input_tokens=40, total_output_tokens=10, total_steps=4)
    _trace(root, "case-2", 1, model="model-a", total_input_tokens=30, total_output_tokens=15, total_steps=3)
    rows = _model_rows(root, tmp_path / "report")
    assert [row["模型"] for row in rows] == ["model-a", "model-b"]
    assert rows[0] == {
        "模型": "model-a",
        "用例数": "2",
        "执行次数": "2",
        "总输入Token数": "40",
        "总输出Token数": "20",
        "总Token数": "60",
        "平均输入Token数": "20.00",
        "平均输出Token数": "10.00",
        "平均总Token数": "30.00",
        "平均执行步骤数": "2.00",
    }
    assert rows[1]["用例数"] == "1"
    assert rows[1]["执行次数"] == "1"
    assert rows[1]["总Token数"] == "50"


def test_combined_model_session_is_counted_once(tmp_path: Path) -> None:
    root = tmp_path / "traces"
    _trace(
        root, "case-1", 1, models=["model-a", "model-b"], total_input_tokens=100, total_output_tokens=10, total_steps=3
    )
    rows = _model_rows(root, tmp_path / "report")
    assert len(rows) == 1
    assert rows[0]["模型"] == "model-a;model-b"
    assert rows[0]["执行次数"] == "1"
    assert rows[0]["总Token数"] == "110"


def test_unknown_and_zero_usage_traces_are_retained(tmp_path: Path) -> None:
    root = tmp_path / "traces"
    _trace(root, "case-1", 1)
    _trace(root, "case-1", 2, total_input_tokens=0, total_output_tokens=0, total_steps=0)
    rows = _model_rows(root, tmp_path / "report")
    assert len(rows) == 1
    assert rows[0]["模型"] == ""
    assert rows[0]["用例数"] == "1"
    assert rows[0]["执行次数"] == "2"
    assert rows[0]["总Token数"] == "0"
    assert rows[0]["平均总Token数"] == "0.00"


def test_empty_trace_root_exports_headers(tmp_path: Path) -> None:
    root = tmp_path / "traces"
    root.mkdir()
    output = tmp_path / "report"
    assert _model_rows(root, output) == []
    assert (output / "trace_model_summary.csv").read_text(encoding="utf-8").startswith("模型,用例数,执行次数,")


def test_model_export_reuses_analysis_and_keeps_return_paths(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    root = tmp_path / "traces"
    _trace(root, "case-1", 1, model="model-a")
    calls = 0
    original = export.analyze_trace_files

    def counted(*args: Any, **kwargs: Any) -> Any:
        nonlocal calls
        calls += 1
        return original(*args, **kwargs)

    monkeypatch.setattr(export, "analyze_trace_files", counted)
    output = tmp_path / "report"
    detail, summary = export.write_trace_analysis_csvs(root, output)
    assert calls == 1
    assert detail == output / "trace_details"
    assert summary == output / "trace_summary.csv"
    assert (output / "trace_model_summary.csv").is_file()


def test_analysis_cli_reports_generated_model_summary(tmp_path: Path) -> None:
    from typer.testing import CliRunner

    from swe_runner.cli import app

    root = tmp_path / "traces"
    _trace(root, "case-1", 1, model="model-a", total_input_tokens=10, total_output_tokens=5)
    output = tmp_path / "output"
    result = CliRunner().invoke(app, ["analyze-traces", "--trace-root", str(root), "--output", str(output)])
    assert result.exit_code == 0, result.output
    assert "Per-model summary CSV:" in result.output
    assert "trace_model_summary.csv" in result.output.replace("\n", "")
    assert (output / "analyze-traces" / "trace_model_summary.csv").is_file()
