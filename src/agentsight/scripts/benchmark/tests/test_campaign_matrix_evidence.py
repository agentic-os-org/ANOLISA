from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

import pytest

sys.path.insert(0, str(Path(__file__).parents[1] / "campaign"))

import aggregate_report
import campaign_evidence
from test_campaign_evidence import summary


def complete_formal_evidence(repetitions: int) -> tuple[Any, ...]:
    campaign_data = {
        "capacity": {"qps_resolution": 50, "confirm_repetitions": repetitions},
        "matrix": {
            "qps": [100, 200, 300, 400, 500],
            "repetitions": repetitions,
            "warmup_seconds": 180,
            "duration_seconds": 900,
        },
        "soak": {"warmup_seconds": 600, "duration_seconds": 14400},
        "recovery": {
            "repetitions": repetitions,
            "stable_seconds": 600,
            "overload_seconds": 300,
            "recover_seconds": 900,
        },
        "fault": {"duration_seconds": 300, "warmup_seconds": 180},
    }
    capacities = {
        version: {
            "maximum_sustainable_qps": 500,
            "first_failed_qps": 550,
            "safety_limit_reached": False,
            "boundary_confirmed": True,
            "confirmation": {
                "500": ["PASS"] * repetitions,
                "550": ["FAIL"] * repetitions,
            },
        }
        for version in campaign_evidence.VERSIONS
    }
    items = []
    for version in campaign_evidence.VERSIONS:
        for qps in campaign_data["matrix"]["qps"]:
            for repetition in range(1, repetitions + 1):
                items.append(
                    (
                        Path(f"/{version}/{qps}/{repetition}/run-result.json"),
                        {
                            "scenario": "matrix",
                            "version": version,
                            "label": f"qps-{qps}",
                            "qps": qps,
                            "repetition": repetition,
                            "duration_seconds": 900,
                            "warmup_seconds": 180,
                            "evaluation": {"verdict": "PASS"},
                        },
                    )
                )
        items.append(
            (
                Path(f"/{version}/soak/run-result.json"),
                {
                    "scenario": "soak",
                    "version": version,
                    "qps": 400,
                    "label": "qps-400",
                    "duration_seconds": 14400,
                    "warmup_seconds": 600,
                    "evaluation": {"verdict": "PASS"},
                    "summary": summary(),
                },
            )
        )
        for repetition in range(1, repetitions + 1):
            for label, qps, duration in (
                ("stable", 400, 600),
                ("overload", 550, 300),
                ("recover", 400, 900),
            ):
                items.append(
                    (
                        Path(
                            f"/{version}/recovery/{label}/{repetition}/run-result.json"
                        ),
                        {
                            "scenario": "recovery",
                            "version": version,
                            "label": label,
                            "repetition": repetition,
                            "qps": qps,
                            "duration_seconds": duration,
                            "warmup_seconds": 0,
                        },
                    )
                )
        items.append(
            (
                Path(f"/{version}/fault/run-result.json"),
                {
                    "scenario": "fault",
                    "version": version,
                    "qps": 400,
                    "duration_seconds": 300,
                    "warmup_seconds": 180,
                },
            )
        )
    recovery = {
        (version, repetition): {"verdict": "PASS"}
        for version in campaign_evidence.VERSIONS
        for repetition in range(1, repetitions + 1)
    }
    faults = {version: {"verdict": "PASS"} for version in campaign_evidence.VERSIONS}
    regression = {
        "full": True,
        "checks": [
            {
                "command": command,
                "exit_code": 0,
            }
            for command in (
                "python -m diff_cover.diff_cover_tool coverage.xml --fail-under=85",
                "cargo fmt --all -- --check",
                "cargo clippy --workspace --all-targets -- -D warnings",
                "cargo test --workspace",
            )
        ],
    }
    return campaign_data, items, capacities, recovery, faults, regression


@pytest.mark.parametrize("repetitions", [1, 3])
@pytest.mark.parametrize(
    "matrix_inputs", ["matching", "baseline_drift", "both_drift", "missing"]
)
def test_campaign_matrix_inputs_match_frozen_qps(
    tmp_path: Path,
    repetitions: int,
    matrix_inputs: str,
) -> None:
    campaign_data, items, capacities, recovery, faults, regression = (
        complete_formal_evidence(repetitions)
    )
    campaign_data["versions"] = {version: {} for version in campaign_evidence.VERSIONS}
    matrix_runs = [run for _, run in items if run["scenario"] == "matrix"]
    if matrix_inputs == "missing":
        del matrix_runs[0]["qps"]
    elif matrix_inputs != "matching":
        for run in matrix_runs:
            if matrix_inputs == "both_drift" or run["version"] == "baseline":
                run["qps"] = 50
    issues = campaign_evidence.audit_campaign(
        campaign_data, items, capacities, recovery, faults, regression
    )
    expected_issues = {
        "matching": 0,
        "baseline_drift": 5 * repetitions,
        "both_drift": 10 * repetitions,
        "missing": 1,
    }[matrix_inputs]
    assert len(issues) == expected_issues
    assert all("matrix" in issue and "did not pass" in issue for issue in issues)

    # Missing QPS cannot be aggregated; exercise final writing directly in that case.
    rows = [] if matrix_inputs == "missing" else aggregate_report.matrix_rows(items)
    if matrix_inputs == "matching":
        assert len(rows) == 10
        assert all(row["repetitions"] == repetitions for row in rows)
    elif matrix_inputs == "baseline_drift":
        baseline_rows = [row for row in rows if row["version"] == "baseline"]
        assert [(row["qps"], row["repetitions"]) for row in baseline_rows] == [
            (50, 5 * repetitions)
        ]
    elif matrix_inputs == "both_drift":
        assert len(rows) == 2
        assert all(
            row["qps"] == 50 and row["repetitions"] == 5 * repetitions for row in rows
        )
    (tmp_path / "manifest.json").write_text("{}", encoding="utf-8")
    final = aggregate_report.write_final(
        tmp_path, campaign_data, rows, capacities, items, recovery, faults, regression
    )
    verdict = "PASS" if matrix_inputs == "matching" else "INCONCLUSIVE"
    assert final["verdict"] == verdict
    assert final["issues"] == issues
    assert (
        json.loads((tmp_path / "final-summary.json").read_text())["verdict"] == verdict
    )
    assert f"**总判定：{verdict}**" in (tmp_path / "final-report.md").read_text(
        encoding="utf-8"
    )
