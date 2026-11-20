# SPDX-License-Identifier: Apache-2.0
"""Special float values must remain visible without aborting a whole report."""

import csv
import importlib.util
import io
import os
import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/summarize_results.py"
SPEC = importlib.util.spec_from_file_location("summary_numeric_format", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


@pytest.mark.parametrize(
    "value, expected",
    [(float("nan"), "nan"), (float("inf"), "inf"), (float("-inf"), "-inf")],
)
def test_nonfinite_values_reach_float_rendering(value: float, expected: str) -> None:
    assert MODULE.fmt(value) == expected


@pytest.mark.parametrize(
    "value, expected",
    [
        (None, "N/A"),
        (True, "Y"),
        (False, "N"),
        (0, "0"),
        (7.0, "7"),
        (1.2345, "1.23"),
        (999999.0, "999999"),
        (1000000.0, "1000000.00"),
        (-1000000.0, "-1000000.00"),
        ("unknown", "unknown"),
    ],
)
def test_existing_numeric_and_missing_value_spelling(
    value: object, expected: str
) -> None:
    assert MODULE.fmt(value) == expected


@pytest.mark.parametrize("format_name", ["csv", "table"])
def test_large_json_exponents_do_not_abort_the_cli(
    tmp_path: Path, format_name: str
) -> None:
    source = tmp_path / "batch.json"
    source.write_text(
        '[{"task_id":"T001","task_name":"Example","difficulty":"hard",'
        '"trials":[{"input_tokens":1e400,"output_tokens":-1e400,"passed":false}]}]',
        encoding="utf-8",
    )
    original = source.read_bytes()
    output = tmp_path / f"summary.{format_name}"
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--input",
            str(source),
            "--format",
            format_name,
            "--output",
            str(output),
        ],
        env={**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"},
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
        timeout=20,
    )
    assert result.returncode == 0, result.stderr
    assert source.read_bytes() == original
    rendered = output.read_text(encoding="utf-8")
    if format_name == "csv":
        rows = list(csv.reader(io.StringIO(rendered)))
        assert rows[1][5:7] == ["inf", "-inf"]
    else:
        assert "inf" in rendered and "-inf" in rendered
        assert "Trials: 1 total" in rendered
