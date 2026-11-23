"""Exercise explicit token-audit run selection through the real CLI."""

import json
import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/compare_session_trace_tokens.py"


def invoke(root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--root", str(root), *args],
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )


def write_pair(root: Path, name: str) -> None:
    run = root / name
    sessions = run / "sessions"
    sessions.mkdir(parents=True)
    session = {"message": {"role": "assistant", "usage": {"input": 10, "output": 3}}}
    trace = {
        "type": "message",
        "message": {"role": "assistant"},
        "usage": {"input_tokens": 10, "output_tokens": 3},
    }
    (sessions / "trial.session.jsonl").write_text(json.dumps(session) + "\n", encoding="utf-8")
    (run / "trial.jsonl").write_text(json.dumps(trace) + "\n", encoding="utf-8")


@pytest.mark.parametrize("kind", ["missing", "file", "empty", "empty-sessions"])
def test_invalid_selected_run_fails_before_export(tmp_path: Path, kind: str) -> None:
    selected = tmp_path / kind
    if kind == "file":
        selected.write_text("not a directory", encoding="utf-8")
    elif kind == "empty":
        selected.mkdir()
    elif kind == "empty-sessions":
        (selected / "sessions").mkdir(parents=True)
    output = tmp_path / "audit.csv"
    output.write_text("existing audit", encoding="utf-8")
    result = invoke(tmp_path, "--run", kind, "--csv", str(output))
    assert result.returncode != 0
    assert kind in result.stderr
    assert "=== Summary ===" not in result.stdout
    assert output.read_text(encoding="utf-8") == "existing audit"


@pytest.mark.parametrize("name", ["", ".", "..", "nested/run"])
def test_run_must_be_a_direct_child_name(tmp_path: Path, name: str) -> None:
    write_pair(tmp_path, "nested/run")
    result = invoke(tmp_path, "--run", name)
    assert result.returncode != 0
    assert "run" in result.stderr.lower()


def test_absolute_run_path_is_rejected(tmp_path: Path) -> None:
    write_pair(tmp_path, "run")
    result = invoke(tmp_path, "--run", str(tmp_path / "run"))
    assert result.returncode != 0
    assert "run" in result.stderr.lower()


def test_valid_selection_and_multi_run_scan(tmp_path: Path) -> None:
    write_pair(tmp_path, "run with spaces")
    write_pair(tmp_path, "other")
    (tmp_path / "empty").mkdir()
    selected = invoke(tmp_path, "--run", "run with spaces")
    assert selected.returncode == 0, selected.stderr
    assert "total_pairs: 1" in selected.stdout
    assert "matched: 1" in selected.stdout
    scanned = invoke(tmp_path)
    assert scanned.returncode == 0, scanned.stderr
    assert "total_pairs: 2" in scanned.stdout
    assert "matched: 2" in scanned.stdout


def test_unselected_empty_root_still_reports_zero(tmp_path: Path) -> None:
    result = invoke(tmp_path)
    assert result.returncode == 0, result.stderr
    assert "total_pairs: 0" in result.stdout


def test_missing_trace_remains_a_discrepancy(tmp_path: Path) -> None:
    write_pair(tmp_path, "run")
    (tmp_path / "run/trial.jsonl").unlink()
    result = invoke(tmp_path, "--run", "run")
    assert result.returncode == 0, result.stderr
    assert "missing_trace: 1" in result.stdout
