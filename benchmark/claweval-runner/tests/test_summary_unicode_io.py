# SPDX-License-Identifier: Apache-2.0
"""Summaries retain Unicode with locale decoding and ASCII stdout enabled."""

import csv
import io
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/summarize_results.py"
TITLE = "邮件 € 😀"


def batch_file(path: Path, title: str = TITLE, escaped: bool = True) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(
            [
                {
                    "task_id": "T001",
                    "task_name": title,
                    "difficulty": "hard",
                    "trials": [{"trace": "T001_abc.jsonl", "passed": False}],
                },
            ],
            ensure_ascii=escaped,
        ),
        encoding="utf-8",
    )
    return path


def run_summary(*arguments: str) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), *map(str, arguments)],
        env={
            **os.environ,
            "PYTHONUTF8": "0",
            "PYTHONCOERCECLOCALE": "0",
            "PYTHONIOENCODING": "ascii",
            "LC_ALL": "C",
        },
        capture_output=True,
        check=False,
        timeout=20,
    )


@pytest.mark.parametrize("format_name", ["csv", "table"])
@pytest.mark.parametrize("to_file", [False, True])
def test_unicode_output_does_not_depend_on_locale(
    tmp_path: Path,
    format_name: str,
    to_file: bool,
) -> None:
    source = batch_file(tmp_path / "input.json")
    original = source.read_bytes()
    destination = tmp_path / "报告_😀.txt"
    arguments = ["--input", str(source), "--format", format_name]
    if to_file:
        arguments += ["--output", str(destination)]
    result = run_summary(*arguments)
    assert result.returncode == 0, result.stderr.decode("utf-8", "replace")
    text = (destination.read_bytes() if to_file else result.stdout).decode("utf-8")
    assert TITLE in text
    if format_name == "csv":
        assert list(csv.reader(io.StringIO(text)))[1][1] == TITLE
    assert source.read_bytes() == original


def test_raw_utf8_batch_is_decoded_as_utf8(tmp_path: Path) -> None:
    source = batch_file(tmp_path / "input.json", escaped=False)
    result = run_summary("--input", str(source), "--format", "csv")
    assert result.returncode == 0, result.stderr.decode("utf-8", "replace")
    assert list(csv.reader(io.StringIO(result.stdout.decode("utf-8"))))[1][1] == TITLE


def test_raw_utf8_reports_retain_failure_text(tmp_path: Path) -> None:
    source = batch_file(tmp_path / "input.json", title="ASCII control")
    reports = tmp_path / "reports"
    reports.mkdir()
    report = reports / "T001_abc.json"
    report.write_text(
        json.dumps(
            {
                "trace_file": "T001_abc.jsonl",
                "status": "fail",
                "failure_classification": {
                    "category": "tool_error",
                    "key_reason_zh": TITLE,
                },
            },
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )
    original = report.read_bytes()
    result = run_summary(
        "--input", str(source), "--report-dir", str(reports), "--format", "csv"
    )
    assert result.returncode == 0, result.stderr.decode("utf-8", "replace")
    rows = list(csv.reader(io.StringIO(result.stdout.decode("utf-8"))))
    assert rows[1][-1] == f"tool_error | {TITLE}"
    assert report.read_bytes() == original


def test_utf8_config_can_select_a_unicode_trace_directory(tmp_path: Path) -> None:
    config = tmp_path / "claw-eval/config.yaml"
    config.parent.mkdir()
    folder = "traces_目录_€_😀"
    config.write_text(f'defaults:\n  trace_dir: "{folder}"\n', encoding="utf-8")
    batch_file(tmp_path / folder / "run/batch_results.json", title="Config control")
    result = run_summary("--config", str(config), "--format", "csv")
    assert result.returncode == 0, result.stderr.decode("utf-8", "replace")
    rows = list(csv.reader(io.StringIO(result.stdout.decode("utf-8"))))
    assert rows[1][1] == "Config control"


def test_ascii_csv_and_newlines_are_unchanged(tmp_path: Path) -> None:
    source = batch_file(tmp_path / "input.json", title="ASCII control")
    output = tmp_path / "output.csv"
    result = run_summary(
        "--input", str(source), "--format", "csv", "--output", str(output)
    )
    assert result.returncode == 0, result.stderr.decode("utf-8", "replace")
    rows = list(csv.reader(io.StringIO(output.read_bytes().decode("utf-8"))))
    assert len(rows) == 2
    assert rows[1][1] == "ASCII control"
    assert b"\r\r\n" not in output.read_bytes()
