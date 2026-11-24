# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.

"""Exercise output paths through the three L4 report commands."""

import hashlib
import json
import subprocess
import sys
from pathlib import Path

import pytest

ASSETS = Path(__file__).resolve().parents[1] / "assets"


@pytest.fixture
def report_inputs(tmp_path: Path) -> tuple[Path, Path]:
    runs, evals = tmp_path / "runs", tmp_path / "evaluations"
    repeat = runs / "host1" / "out" / "repeat-1"
    run = repeat / "arm-a" / "run"
    (run / "results").mkdir(parents=True)
    (repeat / "sweep-summary.txt").write_text(
        "[sweep] sweep repeat=1 arms=a instances=1\n"
        "[sweep] all arms produced evidence-backed results\n",
        encoding="utf-8",
    )
    (run / "results" / "case.json").write_text(
        '{"instance_id":"case","success":true,"patch_produced":true}',
        encoding="utf-8",
    )
    (run.parent / "report.json").write_text(
        '{"instances":{"case":{"prompt_tokens":12,"requests":1}}}', encoding="utf-8"
    )
    (run / "run_metadata.json").write_text(
        json.dumps(
            {
                "instance_ids": ["case"],
                "started_at": "2025-01-01T23:59:00Z",
                "ended_at": "2025-01-02T00:00:00Z",
            }
        ),
        encoding="utf-8",
    )
    predictions = b"{}"
    (run / "preds.json").write_bytes(predictions)
    evaluation = evals / "host1" / "repeat-1-arm-a"
    (evaluation / "evaluate").mkdir(parents=True)
    (evaluation / ".eval-complete").write_text(
        f"{hashlib.sha256(predictions).hexdigest()} run-1\n", encoding="utf-8"
    )
    (evaluation / "evaluate" / "model.run-1.json").write_text(
        '{"submitted_instances":1,"resolved_ids":["case"]}', encoding="utf-8"
    )
    return runs, evals


@pytest.mark.parametrize("script", ("analyze_runs", "analyze_reproducibility", "analyze_solve_rate"))
def test_report_accepts_filename_without_parent(
    tmp_path: Path, report_inputs: tuple[Path, Path], script: str
) -> None:
    runs, evals = report_inputs
    command = [sys.executable, str(ASSETS / "scripts" / f"{script}.py"), "--runs", str(runs)]
    if script == "analyze_runs":
        command.extend(("--configs-dir", str(ASSETS / "configs")))
    elif script == "analyze_reproducibility":
        command.extend(("--repeats", "1"))
    else:
        command.extend(("--evals", str(evals)))

    def read_report(output: str) -> dict:
        result = subprocess.run(
            [*command, "--out", output], cwd=tmp_path, capture_output=True, text=True
        )
        assert result.returncode == 0, result.stdout + result.stderr
        return json.loads((tmp_path / output).read_text(encoding="utf-8"))

    nested = read_report("reports/summary.json")
    assert read_report("summary.json") == nested
