"""Exact instance selection filters trace input before analysis and CSV export."""

import csv
import json
from pathlib import Path

import pytest
from typer.testing import CliRunner

from swe_runner.cli import app
from swe_runner.trace_extraction.analysis import analyze_trace_files
from swe_runner.trace_extraction.export import _SUMMARY_COLUMNS, write_trace_analysis_csvs
from swe_runner.trace_extraction.helpers import ExtractionError


@pytest.fixture
def traces(tmp_path: Path):
    root = tmp_path / "traces"
    for instance, count in [("i1", 2), ("i2", 1)]:
        directory = root / instance
        directory.mkdir(parents=True)
        for trial in range(count):
            (directory / f"trace_{trial}.json").write_text(
                json.dumps(
                    {
                        "session_id": f"{instance}-{trial}",
                        "total_input_tokens": 10,
                        "total_output_tokens": 3,
                        "total_steps": 1,
                    }
                ),
                encoding="utf-8",
            )
    return root, tmp_path / "output"


def test_selected_instance_keeps_all_of_its_traces_once(traces):
    root, _ = traces
    per_trace, per_case = analyze_trace_files(root, instance_ids=["i1", "i1"])
    assert len(per_trace) == 2
    assert {row["instance_id"] for row in per_trace} == {"i1"}
    assert len(per_case) == 1
    assert per_case[0]["execution_count"] == 2


@pytest.mark.parametrize("selection", [["missing"], ["i1", "missing"], [], [""], ["  "], [None]])
def test_invalid_selection_fails_before_csv_mutation(traces, selection):
    root, output = traces
    output.mkdir()
    previous = output / "trace_summary.csv"
    previous.write_text("previous evidence", encoding="utf-8")
    with pytest.raises(ExtractionError, match="[Ii]nstance|traces"):
        write_trace_analysis_csvs(root, output, instance_ids=selection)
    assert previous.read_text(encoding="utf-8") == "previous evidence"
    assert sorted(path.name for path in output.iterdir()) == ["trace_summary.csv"]


def test_explicit_file_selection_is_intersected_with_instance_selection(traces):
    root, _ = traces
    selected = root / "i1/trace_0.json"
    per_trace, _ = analyze_trace_files(root, trace_files=[selected], instance_ids=["i1"])
    assert [row["task_id"] for row in per_trace] == ["i1-0"]
    with pytest.raises(ExtractionError):
        analyze_trace_files(root, trace_files=[selected], instance_ids=["i2"])


def test_unselected_corrupt_trace_does_not_affect_analysis(traces):
    root, output = traces
    (root / "i2/trace_0.json").write_text("{invalid}", encoding="utf-8")
    write_trace_analysis_csvs(root, output, instance_ids=["i1"])
    with (output / "trace_summary.csv").open(encoding="utf-8", newline="") as stream:
        rows = list(csv.DictReader(stream))
    assert [row[_SUMMARY_COLUMNS[0][0]] for row in rows] == ["i1"]


def test_cli_filters_all_exported_views(traces):
    root, output = traces
    result = CliRunner().invoke(
        app,
        [
            "analyze-traces",
            "--trace-root",
            str(root),
            "--output",
            str(output),
            "--instance-id",
            " i1,i1 ",
        ],
    )
    assert result.exit_code == 0, result.output
    artifacts = output / "analyze-traces"
    assert [file.name for file in (artifacts / "trace_details").iterdir()] == ["i1.csv"]
    with (artifacts / "trace_summary.csv").open(encoding="utf-8", newline="") as stream:
        rows = list(csv.DictReader(stream))
    assert [row[_SUMMARY_COLUMNS[0][0]] for row in rows] == ["i1"]
    with (artifacts / "trace_metrics/trace_metrics.csv").open(encoding="utf-8", newline="") as stream:
        rows = list(csv.DictReader(stream))
    assert len(rows) == 2
    assert {row[_SUMMARY_COLUMNS[0][0]] for row in rows} == {"i1"}


@pytest.mark.parametrize("selection", ["missing", "", "i1,", ",i1"])
def test_cli_reports_invalid_selection(traces, selection):
    root, output = traces
    result = CliRunner().invoke(
        app, ["analyze-traces", "--trace-root", str(root), "--output", str(output), "-i", selection]
    )
    assert result.exit_code == 1, result.output
    assert "instance" in result.output.lower() or "traces" in result.output.lower()
    assert not (output / "analyze-traces/trace_summary.csv").exists()


def test_default_analysis_retains_all_instances(traces):
    root, _ = traces
    per_trace, per_case = analyze_trace_files(root)
    assert len(per_trace) == 3
    assert [row["instance_id"] for row in per_case] == ["i1", "i2"]
