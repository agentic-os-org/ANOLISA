"""Compare recorded task outcomes without agents, datasets or judge calls."""

import csv
import importlib.util
import io
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).parents[1] / "scripts/compare_batch_results.py"
COLUMNS = [
    "task_id",
    "outcome_change",
    "baseline_status",
    "candidate_status",
    "baseline_score",
    "candidate_score",
    "score_delta",
    "baseline_trials",
    "candidate_trials",
    "baseline_error_trials",
    "candidate_error_trials",
]


def task(task_id, passed=None, score=None, error=None, trials=None):
    return {
        "task_id": task_id,
        "avg_passed": passed,
        "avg_score": score,
        "error": error,
        "trials": trials,
    }


def invoke(tmp_path: Path, baseline, candidate, *args: str, ascii_io=False):
    before, after = tmp_path / "baseline.json", tmp_path / "candidate.json"
    before.write_text(json.dumps(baseline, ensure_ascii=False), encoding="utf-8")
    after.write_text(json.dumps(candidate, ensure_ascii=False), encoding="utf-8")
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--baseline",
            str(before),
            "--candidate",
            str(after),
            *args,
        ],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
        env={**os.environ, **({"PYTHONIOENCODING": "ascii"} if ascii_io else {})},
    )
    return result, before, after


def test_union_outcomes_and_score_deltas_are_stable(tmp_path: Path):
    baseline = [
        task("regress", True, 0.8),
        task("improve", False, 0.5),
        task("same", True, 0.9),
        task("removed", True, 1),
        task("unknown", None, None),
    ]
    candidate = [
        task("same", True, 0.8),
        task("improve", True, 0.9),
        task("regress", False, 0.6),
        task("added", False, 0.2),
        task("unknown", None, None),
    ]
    result, _, _ = invoke(tmp_path, baseline, candidate)
    assert result.returncode == 0, result.stderr
    rows = list(csv.DictReader(io.StringIO(result.stdout)))
    assert list(rows[0]) == COLUMNS
    assert [row["task_id"] for row in rows] == [
        "added",
        "improve",
        "regress",
        "removed",
        "same",
        "unknown",
    ]
    changes = {row["task_id"]: row["outcome_change"] for row in rows}
    assert changes == {
        "added": "added",
        "improve": "improved",
        "regress": "regressed",
        "removed": "removed",
        "same": "unchanged",
        "unknown": "unknown",
    }
    assert (
        next(row for row in rows if row["task_id"] == "regress")["score_delta"]
        == "-0.2"
    )
    assert (
        next(row for row in rows if row["task_id"] == "same")["score_delta"] == "-0.1"
    )
    assert rows[-1]["score_delta"] == ""


def test_overall_errors_and_trial_error_counts_are_distinguished(tmp_path: Path):
    before = task(
        "t", True, 0.9, trials=[{"error": None}, {"error": "old trial failed"}]
    )
    after = task(
        "t", True, 0.7, error="all trials failed", trials=[{"error": "new failure"}]
    )
    result, _, _ = invoke(tmp_path, [before], [after])
    assert result.returncode == 0, result.stderr
    row = next(csv.DictReader(io.StringIO(result.stdout)))
    assert row["baseline_status"] == "pass"
    assert row["candidate_status"] == "error"
    assert row["outcome_change"] == "regressed"
    assert row["baseline_trials"] == "2"
    assert row["baseline_error_trials"] == "1"
    assert row["candidate_trials"] == row["candidate_error_trials"] == "1"


def test_recorded_pass_status_is_not_recomputed_from_score(tmp_path: Path):
    result, _, _ = invoke(tmp_path, [task("t", True, 0.1)], [task("t", False, 0.99)])
    assert result.returncode == 0, result.stderr
    row = next(csv.DictReader(io.StringIO(result.stdout)))
    assert row["outcome_change"] == "regressed"
    assert row["score_delta"] == "0.89"


def test_csv_quotes_unicode_and_multiline_ids_under_ascii_default(tmp_path: Path):
    identifier = '任务🚀,"quoted"\nsecond line'
    result, _, _ = invoke(
        tmp_path,
        [task(identifier, False, 0.2)],
        [task(identifier, True, 0.8)],
        ascii_io=True,
    )
    assert result.returncode == 0, result.stderr
    assert next(csv.DictReader(io.StringIO(result.stdout)))["task_id"] == identifier


def test_empty_batches_emit_headers(tmp_path: Path):
    result, _, _ = invoke(tmp_path, [], [])
    assert result.returncode == 0, result.stderr
    assert list(csv.reader(io.StringIO(result.stdout))) == [COLUMNS]


@pytest.mark.parametrize(
    "bad",
    [
        None,
        {},
        [None],
        [task("")],
        [task("x"), task("x")],
        [task("x", "yes", 0.2)],
        [task("x", True, True)],
        [task("x", True, -1)],
        [task("x", True, float("nan"))],
        [task("x", True, float("inf"))],
        [task("x", True, 0.2, error=True)],
        [task("x", True, 0.2, trials={})],
        [task("x", True, 0.2, trials=[None])],
    ],
)
def test_invalid_source_preserves_previous_output(tmp_path: Path, bad):
    output = tmp_path / "comparison.csv"
    output.write_text("previous comparison", encoding="utf-8")
    result, _, _ = invoke(
        tmp_path, bad, [task("x", True, 0.2)], "--output", str(output)
    )
    assert result.returncode == 1, result.stderr
    assert result.stdout == ""
    assert output.read_text(encoding="utf-8") == "previous comparison"
    assert "baseline.json" in result.stderr


def test_file_export_is_utf8_and_input_bytes_remain_unchanged(tmp_path: Path):
    output = tmp_path / "nested/comparison.csv"
    before = [task("任务", False, 0.1, trials=[])]
    after = [task("任务", True, 0.8, trials=[])]
    result, baseline, candidate = invoke(
        tmp_path, before, after, "--output", str(output), ascii_io=True
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == ""
    assert (
        next(csv.DictReader(io.StringIO(output.read_text(encoding="utf-8"))))["task_id"]
        == "任务"
    )
    assert json.loads(baseline.read_text(encoding="utf-8")) == before
    assert json.loads(candidate.read_text(encoding="utf-8")) == after
    assert list(output.parent.iterdir()) == [output]


def test_output_cannot_replace_an_input(tmp_path: Path):
    result, baseline, _ = invoke(
        tmp_path,
        [task("t", True, 0.8)],
        [],
        "--output",
        str(tmp_path / "baseline.json"),
    )
    assert result.returncode == 1
    assert json.loads(baseline.read_text(encoding="utf-8"))[0]["task_id"] == "t"


def test_large_finite_scores_do_not_create_nonfinite_deltas(tmp_path: Path):
    result, _, _ = invoke(tmp_path, [task("t", True, 1e308)], [task("t", True, 0)])
    assert result.returncode == 0, result.stderr
    assert next(csv.DictReader(io.StringIO(result.stdout)))["score_delta"] == "-1e+308"


def test_integer_score_difference_above_float_precision_is_retained(tmp_path: Path):
    result, _, _ = invoke(
        tmp_path, [task("t", True, 2**53 + 1)], [task("t", True, 2**53)]
    )
    assert result.returncode == 0, result.stderr
    assert next(csv.DictReader(io.StringIO(result.stdout)))["score_delta"] == "-1"


def test_equal_and_subnormal_score_deltas_are_canonical(tmp_path: Path):
    result, _, _ = invoke(
        tmp_path,
        [task("same", True, 0.8), task("tiny", True, 0)],
        [task("same", True, 0.8), task("tiny", True, 5e-324)],
    )
    assert result.returncode == 0, result.stderr
    rows = list(csv.DictReader(io.StringIO(result.stdout)))
    assert rows[0]["score_delta"] == "0"
    assert rows[1]["score_delta"] == "5e-324"


def test_invalid_late_candidate_has_no_partial_publication(tmp_path: Path):
    output = tmp_path / "comparison.csv"
    output.write_text("previous", encoding="utf-8")
    before = [task("a", True, 0.8)]
    after = [task("a", False, 0.5), task("bad", "yes", 0.2)]
    result, _, _ = invoke(tmp_path, before, after, "--output", str(output))
    assert result.returncode == 1
    assert result.stdout == ""
    assert output.read_text(encoding="utf-8") == "previous"
    assert "candidate.json" in result.stderr
    result, _, _ = invoke(tmp_path, before, after)
    assert result.returncode == 1
    assert result.stdout == ""


@pytest.mark.parametrize("link_type", ["hard", "symbolic"])
def test_output_alias_to_candidate_preserves_input(tmp_path: Path, link_type):
    baseline, candidate, output = (
        tmp_path / name for name in ("baseline.json", "candidate.json", "alias.csv")
    )
    baseline.write_text("[]", encoding="utf-8")
    candidate.write_text(json.dumps([task("t", True, 0.8)]), encoding="utf-8")
    try:
        if link_type == "hard":
            os.link(candidate, output)
        else:
            output.symlink_to(candidate)
    except OSError as error:
        pytest.skip(f"link fixture unavailable: {error}")
    original = candidate.read_bytes()
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--baseline",
            str(baseline),
            "--candidate",
            str(candidate),
            "--output",
            str(output),
        ],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )
    assert result.returncode == 1
    assert "aliases" in result.stderr
    assert candidate.read_bytes() == original


def test_publication_failure_preserves_previous_output_and_cleans_temp(
    tmp_path: Path, monkeypatch, capsys
):
    spec = importlib.util.spec_from_file_location("batch_comparison_test", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    monkeypatch.setitem(sys.modules, spec.name, module)
    spec.loader.exec_module(module)
    baseline, candidate = tmp_path / "baseline.json", tmp_path / "candidate.json"
    baseline.write_text("[]", encoding="utf-8")
    candidate.write_text("[]", encoding="utf-8")
    output = tmp_path / "comparison.csv"
    output.write_text("previous", encoding="utf-8")

    def failed(*args):
        raise OSError("cannot publish comparison")

    monkeypatch.setattr(module.os, "replace", failed)
    assert (
        module.main(
            [
                "--baseline",
                str(baseline),
                "--candidate",
                str(candidate),
                "--output",
                str(output),
            ]
        )
        == 1
    )
    assert output.read_text(encoding="utf-8") == "previous"
    assert not list(tmp_path.glob(".batch-comparison-*.tmp"))
    assert "cannot publish" in capsys.readouterr().err


def test_unpaired_surrogate_id_is_rejected_before_stdout(tmp_path: Path):
    baseline, candidate = tmp_path / "baseline.json", tmp_path / "candidate.json"
    baseline.write_text("[]", encoding="utf-8")
    candidate.write_text('[{"task_id":"\\ud800"}]', encoding="utf-8")
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--baseline",
            str(baseline),
            "--candidate",
            str(candidate),
        ],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )
    assert result.returncode == 1
    assert result.stdout == ""


def test_rounded_scientific_delta_omits_insignificant_zeros(tmp_path: Path):
    result, _, _ = invoke(tmp_path, [task("t", True, 1e308)], [task("t", True, 5e-324)])
    assert result.returncode == 0, result.stderr
    assert next(csv.DictReader(io.StringIO(result.stdout)))["score_delta"] == "-1e+308"
