from __future__ import annotations

import csv
import json
import subprocess
import sys
from pathlib import Path

import pytest

from test_campaign_evidence import summary, thresholds, write_recovery_artifacts
from test_campaign_matrix_evidence import complete_formal_evidence

BENCHMARK = Path(__file__).parents[1]


@pytest.mark.parametrize(
    "qps_shape",
    [
        "valid",
        "missing",
        "null",
        "text",
        "object",
        "infinite",
        "fractional",
        "boolean",
        "zero",
        "negative",
    ],
)
def test_matrix_report_cli_preserves_incomplete_evidence(
    tmp_path: Path, qps_shape: str
) -> None:
    settings, items, capacities, _, _, regression = complete_formal_evidence(1)
    config = json.loads((BENCHMARK / "campaign/campaign.example.json").read_text())
    for section, values in settings.items():
        config[section].update(values)
    config["thresholds"] = thresholds()
    config["recovery"].update(tolerance_ratio=0.1, recovery_window_seconds=2)
    config["fault"]["repetitions_per_case"] = 1
    config_path = tmp_path / "campaign.json"
    config_path.write_text(json.dumps(config))
    (tmp_path / "manifest.json").write_text("{}")
    (tmp_path / "regression.json").write_text(json.dumps(regression))
    for version, capacity in capacities.items():
        (tmp_path / f"capacity-{version}.json").write_text(json.dumps(capacity))
    for index, (_, run) in enumerate(items):
        run["summary"] = summary()
        run.setdefault("evaluation", {"verdict": "PASS"})
        run["started_at_unix"] = 0
        path = tmp_path / "runs" / f"run-{index}" / "run-result.json"
        path.parent.mkdir(parents=True)
        if run["scenario"] == "recovery" and run["label"] == "recover":
            write_recovery_artifacts(path)
        if run["scenario"] == "fault":
            import campaign_evidence

            measurement = path.parent / "measurement"
            measurement.mkdir()
            (measurement / "fault-results.json").write_text(
                json.dumps(
                    {
                        "outcomes": {
                            name: {"handled": 1} for name in campaign_evidence.FAULT_CASES
                        },
                        "server_healthy_after": True,
                        "sse_truncation_verified": True,
                        "process_alive_before": True,
                        "process_alive_after": True,
                    }
                )
            )
        if index == 0 and qps_shape != "valid":
            if qps_shape == "missing":
                del run["qps"]
            else:
                run["qps"] = {
                    "null": None,
                    "text": "bad",
                    "object": {},
                    "infinite": float("inf"),
                    "fractional": 100.75,
                    "boolean": True,
                    "zero": 0,
                    "negative": -100,
                }[qps_shape]
        path.write_text(json.dumps(run))
    result = subprocess.run(
        [
            sys.executable,
            str(BENCHMARK / "campaign/aggregate_report.py"),
            "--campaign",
            str(config_path),
            "--results",
            str(tmp_path),
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    final = json.loads((tmp_path / "final-summary.json").read_text())
    verdict = "PASS" if qps_shape == "valid" else "INCONCLUSIVE"
    assert final["verdict"] == verdict
    assert f"**总判定：{verdict}**" in (tmp_path / "final-report.md").read_text()
    inventory = (tmp_path / "run-inventory.csv").read_text()
    assert "runs/run-0/run-result.json" in inventory
    if qps_shape != "valid":
        assert "baseline matrix 100 QPS rep 1 did not pass" in final["issues"]
        with (tmp_path / "performance-comparison.csv").open(
            encoding="utf-8", newline=""
        ) as handle:
            rows = list(csv.DictReader(handle))
        assert len(rows) == 9
        assert not any(
            row["version"] == "baseline" and row["qps"] == "100" for row in rows
        )


def test_report_writers_tolerate_partial_run_records(tmp_path: Path) -> None:
    import aggregate_report

    soak_items = [
        (tmp_path / "soak.json", {"scenario": "soak", "version": "baseline"})
    ]
    aggregate_report.write_soak(tmp_path, soak_items)

    soak_report = (tmp_path / "soak-report.md").read_text()
    assert "| baseline | — |" in soak_report
    assert "NOT EXECUTED" not in soak_report


def test_matrix_rows_skip_records_without_identity() -> None:
    import aggregate_report

    items = [
        (Path("a"), {"scenario": "matrix", "qps": 100}),
        (Path("b"), {"scenario": "matrix", "qps": 100, "version": None}),
    ]
    assert aggregate_report.matrix_rows(items) == []

    null_evaluation = [
        (
            Path("c"),
            {"scenario": "matrix", "qps": 100, "version": "baseline", "evaluation": None},
        )
    ]
    rows = aggregate_report.matrix_rows(null_evaluation)
    assert rows[0]["pass_count"] == 0


@pytest.mark.parametrize("qps", [100, 100.0, "100"])
def test_matrix_rows_keep_exact_integer_levels(qps: int | float | str) -> None:
    import aggregate_report

    run = {"scenario": "matrix", "qps": qps, "version": "baseline"}
    rows = aggregate_report.matrix_rows([(Path("run-result.json"), run)])
    assert [row["qps"] for row in rows] == [100]


def test_final_report_excludes_unusable_checks_from_the_denominator(
    tmp_path: Path,
) -> None:
    import aggregate_report

    settings, items, capacities, recovery, faults, regression = (
        complete_formal_evidence(1)
    )
    config = json.loads((BENCHMARK / "campaign/campaign.example.json").read_text())
    for section, values in settings.items():
        config[section].update(values)
    config["thresholds"] = thresholds()
    config["recovery"].update(tolerance_ratio=0.1, recovery_window_seconds=2)
    config["fault"]["repetitions_per_case"] = 1
    # Four readable checks plus three entries that are not evidence either way;
    # an unreadable entry must leave the denominator alone.
    regression["checks"] = regression["checks"] + [
        {"command": "no-exit-code"},
        {"exit_code": 0},
        "not-an-object",
    ]
    (tmp_path / "manifest.json").write_text("{}")
    aggregate_report.write_final(
        tmp_path, config, [], capacities, items, recovery, faults, regression
    )
    report = (tmp_path / "final-report.md").read_text()
    assert "| 通过命令 | 4/4 |" in report


@pytest.mark.parametrize("bad_evaluation", [["PASS"], "PASS", 1, True])
def test_non_object_evaluation_preserves_inventory_and_matrix_counts(
    tmp_path: Path, bad_evaluation: object
) -> None:
    import aggregate_report

    items = [
        (
            tmp_path / "runs" / "trial-1" / "run-result.json",
            {
                "scenario": "matrix",
                "version": "baseline",
                "label": "qps-100",
                "repetition": 1,
                "qps": 100,
                "summary": {},
                "evaluation": {"verdict": "PASS", "missing": [], "failed": []},
            },
        ),
        (
            tmp_path / "runs" / "trial-2" / "run-result.json",
            {
                "scenario": "matrix",
                "version": "baseline",
                "label": "qps-100",
                "repetition": 2,
                "qps": 100,
                "summary": {},
                "evaluation": bad_evaluation,
            },
        ),
    ]

    aggregate_report.write_run_inventory(tmp_path, items)
    with (tmp_path / "run-inventory.csv").open(encoding="utf-8", newline="") as handle:
        inventory_rows = list(csv.DictReader(handle))
    assert len(inventory_rows) == 2
    assert inventory_rows[0]["verdict"] == "PASS"
    assert inventory_rows[1]["verdict"] == ""
    assert inventory_rows[1]["missing_gates"] == "[]"
    assert inventory_rows[1]["failed_gates"] == "[]"

    rows = aggregate_report.matrix_rows(items)
    assert len(rows) == 1
    assert rows[0]["repetitions"] == 2
    assert rows[0]["pass_count"] == 1


def test_matrix_report_cli_handles_non_object_evaluation_end_to_end(
    tmp_path: Path,
) -> None:
    settings, items, capacities, _, _, regression = complete_formal_evidence(1)
    config = json.loads((BENCHMARK / "campaign/campaign.example.json").read_text())
    for section, values in settings.items():
        config[section].update(values)
    config["thresholds"] = thresholds()
    config["recovery"].update(tolerance_ratio=0.1, recovery_window_seconds=2)
    config["fault"]["repetitions_per_case"] = 1
    config_path = tmp_path / "campaign.json"
    config_path.write_text(json.dumps(config))
    (tmp_path / "manifest.json").write_text("{}")
    (tmp_path / "regression.json").write_text(json.dumps(regression))
    for version, capacity in capacities.items():
        (tmp_path / f"capacity-{version}.json").write_text(json.dumps(capacity))
    for index, (_, run) in enumerate(items):
        run["summary"] = summary()
        run.setdefault("evaluation", {"verdict": "PASS"})
        run["started_at_unix"] = 0
        path = tmp_path / "runs" / f"run-{index}" / "run-result.json"
        path.parent.mkdir(parents=True)
        if run["scenario"] == "recovery" and run["label"] == "recover":
            write_recovery_artifacts(path)
        if run["scenario"] == "fault":
            import campaign_evidence

            measurement = path.parent / "measurement"
            measurement.mkdir()
            (measurement / "fault-results.json").write_text(
                json.dumps(
                    {
                        "outcomes": {
                            name: {"handled": 1} for name in campaign_evidence.FAULT_CASES
                        },
                        "server_healthy_after": True,
                        "sse_truncation_verified": True,
                        "process_alive_before": True,
                        "process_alive_after": True,
                    }
                )
            )
        if index == 0:
            run["evaluation"] = ["PASS"]
        path.write_text(json.dumps(run))

    result = subprocess.run(
        [
            sys.executable,
            str(BENCHMARK / "campaign/aggregate_report.py"),
            "--campaign",
            str(config_path),
            "--results",
            str(tmp_path),
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    final = json.loads((tmp_path / "final-summary.json").read_text())
    assert final["verdict"] == "INCONCLUSIVE"
    assert "baseline matrix 100 QPS rep 1 did not pass" in final["issues"]
    with (tmp_path / "run-inventory.csv").open(encoding="utf-8", newline="") as handle:
        inventory_rows = list(csv.DictReader(handle))
    target = next(r for r in inventory_rows if r["result_path"] == "runs/run-0/run-result.json")
    assert target["verdict"] == ""
    assert target["missing_gates"] == "[]"
    assert target["failed_gates"] == "[]"


def test_audit_campaign_handles_non_object_soak_evaluation() -> None:
    import campaign_evidence

    settings, items, capacities, recovery, faults, regression = complete_formal_evidence(1)
    for _, run in items:
        run["summary"] = summary()
        run.setdefault("evaluation", {"verdict": "PASS"})
        run["started_at_unix"] = 0
        if run["scenario"] == "soak" and run["version"] == "baseline":
            run["evaluation"] = "PASS"

    issues = campaign_evidence.audit_campaign(
        settings, items, capacities, recovery, faults, regression
    )
    assert "baseline soak run did not pass the frozen window" in issues
