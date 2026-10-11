"""Run the offline token comparison command as an automation gate."""

import csv
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).parents[1] / "scripts" / "compare_session_trace_tokens.py"


def write_pair(
    root: Path, run: str = "run", stem: str = "trial", kind: str = "ok"
) -> None:
    directory = root / run
    sessions = directory / "sessions"
    sessions.mkdir(parents=True, exist_ok=True)
    usage = {"input": 10, "output": 3, "totalTokens": 13}
    if kind == "cache_nonzero":
        usage["cacheRead"] = 2
    session = {"message": {"role": "assistant", "usage": usage}}
    (sessions / f"{stem}.session.jsonl").write_text(
        json.dumps(session) + "\n", encoding="utf-8"
    )
    if kind == "missing_trace":
        return
    message = {
        "type": "message",
        "message": {"role": "assistant"},
        "usage": {"input_tokens": 10, "output_tokens": 3},
    }
    if kind in {"input", "output"}:
        message["usage"][f"{kind}_tokens"] += 1
    end = {
        "type": "trace_end",
        "model_input_tokens": message["usage"]["input_tokens"],
        "model_output_tokens": message["usage"]["output_tokens"],
        "total_tokens": 13,
    }
    if kind == "trace_end":
        end["model_input_tokens"] = 100
    records = [message, end] if kind != "assistants" else [end]
    (directory / f"{stem}.jsonl").write_text(
        "".join(json.dumps(row) + "\n" for row in records), encoding="utf-8"
    )


def invoke(root: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--root", str(root), *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )


def test_check_accepts_matching_evidence(tmp_path: Path):
    write_pair(tmp_path)
    result = invoke(tmp_path, "--check", "--detail")
    assert result.returncode == 0, result.stderr
    assert "matched: 1" in result.stdout
    assert "[ok] run/trial" in result.stdout


@pytest.mark.parametrize(
    "kind",
    ["input", "output", "assistants", "trace_end", "cache_nonzero", "missing_trace"],
)
def test_check_fails_existing_discrepancy_rows_after_csv_export(
    tmp_path: Path, kind: str
):
    write_pair(tmp_path, stem="bad", kind=kind)
    write_pair(tmp_path, stem="good")
    output = tmp_path / "audit.csv"
    result = invoke(tmp_path, "--check", "--csv", str(output))
    assert result.returncode == 1, result.stderr
    assert kind in result.stdout
    with output.open(encoding="utf-8", newline="") as stream:
        rows = list(csv.DictReader(stream))
    assert len(rows) == 2
    assert kind in rows[0]["status"]
    assert rows[1]["status"] == "ok"
    assert "CSV written:" in result.stdout


@pytest.mark.parametrize("selected_run", [None, "empty", "missing"])
def test_check_rejects_no_comparable_pairs(tmp_path: Path, selected_run):
    (tmp_path / "empty").mkdir()
    args = () if selected_run is None else ("--run", selected_run)
    result = invoke(tmp_path, "--check", *args)
    assert result.returncode == 1
    assert "no session/trace pairs" in result.stderr.lower()


def test_check_only_considers_selected_run(tmp_path: Path):
    write_pair(tmp_path, run="good")
    write_pair(tmp_path, run="bad", kind="input")
    assert invoke(tmp_path, "--check", "--run", "good").returncode == 0
    assert invoke(tmp_path, "--check").returncode == 1


def test_default_discrepancy_report_remains_informational(tmp_path: Path):
    write_pair(tmp_path, kind="input")
    result = invoke(tmp_path)
    assert result.returncode == 0
    assert "input_mismatch: 1" in result.stdout


def test_default_empty_inventory_remains_informational(tmp_path: Path):
    result = invoke(tmp_path)
    assert result.returncode == 0
    assert "total_pairs: 0" in result.stdout


def test_check_text_remains_utf8_with_ascii_stdout_default(tmp_path: Path):
    write_pair(tmp_path, run="运行🚀", stem="试验", kind="input")
    result = subprocess.run(
        [sys.executable, str(SCRIPT), "--root", str(tmp_path), "--check"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
        env={**os.environ, "PYTHONIOENCODING": "ascii"},
    )
    assert result.returncode == 1
    assert "运行🚀/试验" in result.stdout
    assert "UnicodeEncodeError" not in result.stderr
