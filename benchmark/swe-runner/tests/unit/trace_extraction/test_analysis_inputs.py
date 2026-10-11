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

"""Input file failures use the trace analyzer's actionable error contract."""

import json
from pathlib import Path
from unittest.mock import patch

import pytest

from swe_runner.trace_extraction.analysis import analyze_trace_files
from swe_runner.trace_extraction.helpers import ExtractionError


@pytest.mark.parametrize("content", ["[]", "null", "42", '"text"', "false"])
def test_nonobject_trace_reports_filename(tmp_path: Path, content: str) -> None:
    path = tmp_path / "trace.json"
    path.write_text(content, encoding="utf-8")

    with pytest.raises(ExtractionError, match="object") as error:
        analyze_trace_files(tmp_path, trace_files=[path])

    assert str(path) in str(error.value)


@pytest.mark.parametrize("content", [b"\xff", b"{not json"])
def test_decoding_and_syntax_errors_report_filename(tmp_path: Path, content: bytes) -> None:
    path = tmp_path / "trace.json"
    path.write_bytes(content)

    with pytest.raises(ExtractionError) as error:
        analyze_trace_files(tmp_path, trace_files=[path])

    assert str(path) in str(error.value)
    assert error.value.__cause__ is not None


def test_missing_selected_trace_reports_filename(tmp_path: Path) -> None:
    path = tmp_path / "missing.json"
    with pytest.raises(ExtractionError) as error:
        analyze_trace_files(tmp_path, trace_files=[path])
    assert str(path) in str(error.value)
    assert isinstance(error.value.__cause__, FileNotFoundError)


def test_unreadable_trace_preserves_cause(tmp_path: Path) -> None:
    path = tmp_path / "trace.json"
    with patch.object(Path, "read_text", side_effect=PermissionError("read denied")):
        with pytest.raises(ExtractionError) as error:
            analyze_trace_files(tmp_path, trace_files=[path])
    assert str(path) in str(error.value)
    assert isinstance(error.value.__cause__, PermissionError)


def test_trace_root_must_be_a_directory(tmp_path: Path) -> None:
    path = tmp_path / "root.json"
    path.write_text("{}", encoding="utf-8")
    with pytest.raises(ExtractionError, match="directory") as error:
        analyze_trace_files(path)
    assert str(path) in str(error.value)


def test_valid_trace_results_are_preserved(tmp_path: Path) -> None:
    path = tmp_path / "case" / "trace.json"
    path.parent.mkdir()
    path.write_text(
        json.dumps({"session_id": "session", "models": ["model"], "total_input_tokens": 10, "total_output_tokens": 2, "total_steps": 1}),
        encoding="utf-8",
    )

    details, summaries = analyze_trace_files(tmp_path)

    assert details == [{"instance_id": "case", "task_id": "session", "model": "model", "total_input_tokens": 10, "total_output_tokens": 2, "total_steps": 1}]
    assert summaries[0]["execution_count"] == 1
    assert summaries[0]["avg_total_tokens"] == "12.00"


def test_cli_reports_nonobject_trace_as_normal_error(tmp_path: Path) -> None:
    from typer.testing import CliRunner

    from swe_runner.cli import app

    trace_root = tmp_path / "traces"
    path = trace_root / "case" / "trace.json"
    path.parent.mkdir(parents=True)
    path.write_text("[]", encoding="utf-8")

    result = CliRunner().invoke(app, ["analyze-traces", "--trace-root", str(trace_root), "--output", str(tmp_path / "output")])

    assert result.exit_code == 1
    assert str(path) in result.output.replace("\n", "")
    assert "JSON object" in result.output
    assert "AttributeError" not in result.output
