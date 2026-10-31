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

"""Trace collection selectors must govern all generated CSV artifacts."""

import csv
import json
from pathlib import Path
from types import SimpleNamespace

import pytest
from typer.testing import CliRunner

from swe_runner import cli_commands
from swe_runner.cli import app
from swe_runner.trace_extraction import write_trace_analysis_csvs


def _trace(root: Path, case: str, name: str, tokens: int) -> Path:
    parent = root / case
    parent.mkdir(parents=True, exist_ok=True)
    path = parent / f"trace-{name}.json"
    path.write_text(
        json.dumps(
            {
                "session_id": name,
                "model": "example-model",
                "total_input_tokens": tokens,
                "total_output_tokens": 2,
                "total_steps": 1,
            }
        ),
        encoding="utf-8",
    )
    return path


def _csv(path: Path) -> list[dict[str, str]]:
    with path.open(encoding="utf-8", newline="") as stream:
        return list(csv.DictReader(stream))


@pytest.fixture
def traces(tmp_path: Path) -> tuple[Path, Path]:
    root = tmp_path / "traces"
    _trace(root, "historical-case", "old-other", 999)
    _trace(root, "selected-case", "old-session", 500)
    selected = _trace(root, "selected-case", "current-session", 10)
    return root, selected


def _run_cli(
    root: Path,
    output: Path,
    selection: list[Path] | None,
    monkeypatch: pytest.MonkeyPatch,
) -> Path:
    monkeypatch.setattr(
        cli_commands.TraceCollectionPlan,
        "resolve",
        lambda **kwargs: SimpleNamespace(collect=lambda destination: selection),
    )
    arguments = ["analyze-traces", "--trace-root", str(root), "--output", str(output)]
    if selection is not None:
        arguments.extend(["--start", "2026-01-01T00:00:00Z"])
    result = CliRunner().invoke(app, arguments)
    assert result.exit_code == 0, result.output
    if selection is not None:
        assert f"Recorded traces: {len(selection)}" in result.output
    return output / "analyze-traces"


def _assert_selected(output: Path) -> None:
    summary = _csv(output / "trace_summary.csv")
    assert len(summary) == 1
    assert summary[0]["用例ID"] == "selected-case"
    assert summary[0]["执行次数"] == "1"
    assert summary[0]["平均输入Token数"] == "10.00"
    details = list((output / "trace_details").glob("*.csv"))
    assert len(details) == 1
    assert _csv(details[0])[0]["任务ID"] == "current-session"
    metrics = _csv(output / "trace_metrics" / "trace_metrics.csv")
    assert [(row["用例ID"], row["任务ID"]) for row in metrics] == [
        ("selected-case", "current-session")
    ]


def test_cli_exports_only_current_collection(
    traces: tuple[Path, Path], tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root, selected = traces
    output = _run_cli(root, tmp_path / "output", [selected], monkeypatch)
    _assert_selected(output)
    assert len(list(root.glob("*/trace*.json"))) == 3


def test_empty_collection_exports_no_historical_rows(
    traces: tuple[Path, Path], tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root, _ = traces
    output = _run_cli(root, tmp_path / "output", [], monkeypatch)
    assert _csv(output / "trace_summary.csv") == []
    assert _csv(output / "trace_metrics" / "trace_metrics.csv") == []
    assert list((output / "trace_details").glob("*.csv")) == []
    assert len(list(root.glob("*/trace*.json"))) == 3


def test_offline_analysis_still_scans_the_whole_root(
    traces: tuple[Path, Path], tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root, _ = traces
    output = _run_cli(root, tmp_path / "output", None, monkeypatch)
    summary = _csv(output / "trace_summary.csv")
    assert {row["用例ID"]: row["执行次数"] for row in summary} == {
        "historical-case": "1",
        "selected-case": "2",
    }
    assert len(_csv(output / "trace_metrics" / "trace_metrics.csv")) == 3


def test_unselected_invalid_history_cannot_abort_current_report(
    traces: tuple[Path, Path], tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root, selected = traces
    (root / "historical-case" / "trace-invalid.json").write_text("not JSON", encoding="utf-8")
    output = _run_cli(root, tmp_path / "output", [selected], monkeypatch)
    _assert_selected(output)


def test_export_facade_accepts_an_explicit_collection(
    traces: tuple[Path, Path], tmp_path: Path
) -> None:
    root, selected = traces
    output = tmp_path / "export"
    write_trace_analysis_csvs(root, output, trace_files=[selected])
    _assert_selected(output)


def test_empty_explicit_collection_does_not_require_a_trace_root(tmp_path: Path) -> None:
    output = tmp_path / "export"
    write_trace_analysis_csvs(tmp_path / "absent", output, trace_files=[])
    assert _csv(output / "trace_summary.csv") == []
    assert _csv(output / "trace_metrics" / "trace_metrics.csv") == []
