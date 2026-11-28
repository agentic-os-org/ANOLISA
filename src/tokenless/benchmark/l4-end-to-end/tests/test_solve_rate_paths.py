# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.

import hashlib
import json
import subprocess
import sys
from pathlib import Path

import pytest

SOLVE_RATE = (
    Path(__file__).resolve().parents[1] / "assets" / "scripts" / "analyze_solve_rate.py"
)


def write_json(path: Path, data: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data), encoding="utf-8")


def seed_run_tree(evals_root: Path, runs_root: Path, case_name: str) -> None:
    repeat = runs_root / "host1" / "out" / "repeat-1"
    solved = f"{case_name}-solved"
    predictions = repeat / "arm-a" / "run" / "preds.json"
    write_json(predictions, [{"instance_id": solved}])
    write_json(
        repeat / "arm-a" / "selection.json",
        {"instance_ids": [solved, f"{case_name}-timeout"]},
    )
    write_json(
        repeat / "arm-b" / "selection.json", {"instance_ids": [f"{case_name}-pending"]}
    )
    evaluation = evals_root / "host1" / "eval" / "repeat-1-arm-a"
    write_json(
        evaluation / "evaluate" / "model.local-run.json",
        {"resolved_ids": [solved], "submitted_instances": 1, "completed_instances": 1},
    )
    digest = hashlib.sha256(predictions.read_bytes()).hexdigest()
    (evaluation / ".eval-complete").write_text(
        f"{digest} local-run\n", encoding="utf-8"
    )


def solve_rate_cells(
    work: Path, evals: str, runs: str | None = None
) -> dict[str, dict[str, object]]:
    output = Path("reports") / "solve_rate.json"
    command = [
        sys.executable,
        "-X",
        "utf8",
        str(SOLVE_RATE),
        "--evals",
        evals,
        "--out",
        str(output),
    ]
    if runs is not None:
        command.extend(("--runs", runs))
    result = subprocess.run(
        command,
        cwd=work,
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=15,
        check=False,
    )
    assert result.returncode == 0, result.stderr or result.stdout
    report = json.loads((work / output).read_text(encoding="utf-8"))
    return report["per_repeat_arm"]


def assert_target_cells(cells: dict[str, dict[str, object]]) -> None:
    assert set(cells) == {"repeat1-arma", "repeat1-armb"}
    scored = cells["repeat1-arma"]
    assert scored["selected"] == 2
    assert scored["resolved"] == 1
    assert scored["solve_rate_pct"] == 50.0
    assert scored["unresolved_ids"] == ["target-timeout"]
    pending = cells["repeat1-armb"]
    assert pending["selected"] == 1
    assert pending["unscored_ids"] == ["target-pending"]
    assert pending["solve_rate_pct"] is None


@pytest.mark.parametrize(
    ("evals", "runs"),
    [
        pytest.param("evals", "runs", id="plain"),
        pytest.param("evals [ab]", "runs", id="bracketed-evals"),
        pytest.param("evals", "runs [ab]", id="bracketed-runs"),
        pytest.param("evals [ab]", "runs [ab]", id="both-bracketed"),
    ],
)
def test_solve_rate_reads_explicit_literal_roots(
    tmp_path: Path, evals: str, runs: str
) -> None:
    seed_run_tree(tmp_path / evals, tmp_path / runs, "target")

    assert_target_cells(solve_rate_cells(tmp_path, evals, runs))


@pytest.mark.parametrize(
    "evals_suffix",
    [
        pytest.param(Path(), id="collected-root"),
        pytest.param(Path("host1") / "eval", id="per-host-eval"),
    ],
)
def test_solve_rate_infers_the_literal_run_root(
    tmp_path: Path, evals_suffix: Path
) -> None:
    collected = Path("collected [ab]")
    requested = tmp_path / collected
    sibling = tmp_path / "collected a"
    seed_run_tree(requested, requested, "target")
    seed_run_tree(sibling, sibling, "neighbor")

    assert_target_cells(solve_rate_cells(tmp_path, str(collected / evals_suffix)))
