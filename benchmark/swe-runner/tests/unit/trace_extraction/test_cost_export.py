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

"""Recorded-cost reports distinguish missing costs from known zero costs."""

import csv
import json
from pathlib import Path
from typing import Any

import pytest

from swe_runner.trace_extraction.analysis import analyze_trace_files
from swe_runner.trace_extraction.export import write_trace_analysis_csvs
from swe_runner.trace_extraction.helpers import ExtractionError


def _trace(root: Path, index: int, cost: Any) -> None:
    directory = root / "case"
    directory.mkdir(parents=True, exist_ok=True)
    data = {
        "session_id": f"session-{index}",
        "total_input_tokens": 10,
        "total_output_tokens": 2,
        "total_steps": 1,
        "total_cost": cost,
    }
    (directory / f"trace-{index}.json").write_text(json.dumps(data), encoding="utf-8")


def test_known_costs_and_partial_coverage_are_aggregated(tmp_path: Path) -> None:
    for index, cost in enumerate([0.125, 0.25, 0, None]):
        _trace(tmp_path, index, cost)

    details, summaries = analyze_trace_files(tmp_path, include_metrics=True)

    assert [row["total_cost"] for row in details] == ["0.125", "0.25", "0.0", ""]
    assert summaries[0]["recorded_cost_count"] == 3
    assert summaries[0]["total_recorded_cost"] == "0.375"
    assert summaries[0]["avg_recorded_cost"] == "0.125"
    assert summaries[0]["execution_count"] == 4


@pytest.mark.parametrize("cost", [None, "unknown", True, float("nan"), float("inf")])
def test_unusable_costs_keep_unknown_totals(tmp_path: Path, cost: Any) -> None:
    _trace(tmp_path, 0, cost)
    details, summaries = analyze_trace_files(tmp_path, include_metrics=True)
    assert details[0]["total_cost"] == ""
    assert summaries[0]["recorded_cost_count"] == 0
    assert summaries[0]["total_recorded_cost"] == ""
    assert summaries[0]["avg_recorded_cost"] == ""


def test_lightweight_analysis_contract_remains_unchanged(tmp_path: Path) -> None:
    _trace(tmp_path, 0, 0.125)
    details, summaries = analyze_trace_files(tmp_path)
    assert "total_cost" not in details[0]
    assert "recorded_cost_count" not in summaries[0]


def test_export_writes_cost_headers_and_empty_unknown_cells(tmp_path: Path) -> None:
    root = tmp_path / "traces"
    _trace(root, 0, 0.125)
    _trace(root, 1, None)
    output = tmp_path / "reports"

    detail_dir, summary_path = write_trace_analysis_csvs(root, output)

    for path in [detail_dir / "case.csv", output / "trace_metrics" / "trace_metrics.csv"]:
        with path.open(encoding="utf-8", newline="") as stream:
            rows = list(csv.DictReader(stream))
        assert [row["记录总成本"] for row in rows] == ["0.125", ""]
    with summary_path.open(encoding="utf-8", newline="") as stream:
        rows = list(csv.DictReader(stream))
    assert rows[0]["成本记录次数"] == "1"
    assert rows[0]["记录成本合计"] == "0.125"
    assert rows[0]["平均记录成本"] == "0.125"


@pytest.mark.parametrize("costs", [[1e308, 1e308], [1e308, 1e308, -1e308, -1e308]])
def test_cost_aggregation_overflow_reports_instance_without_writing_reports(tmp_path: Path, costs: list[float]) -> None:
    root = tmp_path / "traces"
    for index, cost in enumerate(costs):
        _trace(root, index, cost)
    output = tmp_path / "reports"
    with pytest.raises(ExtractionError, match="cost.*case.*finite") as error:
        write_trace_analysis_csvs(root, output)
    assert isinstance(error.value.__cause__, OverflowError)
    assert not output.exists()
    for index, cost in enumerate(costs):
        assert json.loads((root / "case" / f"trace-{index}.json").read_text(encoding="utf-8"))["total_cost"] == cost
