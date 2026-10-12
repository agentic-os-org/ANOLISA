from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

from test_campaign_evidence import summary, thresholds, write_recovery_artifacts
from test_campaign_matrix_evidence import complete_formal_evidence

BENCHMARK = Path(__file__).parents[1]


@pytest.mark.parametrize(
    "shape",
    [
        "valid",
        "overload-missing",
        "recover-missing",
        "overload-null",
        "recover-null",
        "overload-text",
        "recover-text",
        "overload-absent",
        "recover-absent",
    ],
)
def test_recovery_report_cli_preserves_incomplete_evidence(
    tmp_path: Path, shape: str
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
                            name: {"handled": 1}
                            for name in campaign_evidence.FAULT_CASES
                        },
                        "server_healthy_after": True,
                        "process_alive_before": True,
                        "process_alive_after": True,
                    }
                )
            )
        if (
            run["scenario"] == "recovery"
            and run["version"] == "baseline"
            and shape != "valid"
        ):
            label, damage = shape.split("-")
            if run["label"] == label:
                if damage == "absent":
                    continue
                if damage == "missing":
                    del run["summary"]
                else:
                    run["summary"] = None if damage == "null" else "bad"
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
    verdict = "PASS" if shape == "valid" else "INCONCLUSIVE"
    assert final["verdict"] == verdict
    assert f"**总判定：{verdict}**" in (tmp_path / "final-report.md").read_text()
    inventory = (tmp_path / "run-inventory.csv").read_text()
    assert "runs/run-0/run-result.json" in inventory
    if shape != "valid":
        assert "baseline recovery rep 1 did not pass" in final["issues"]
        recovery = (tmp_path / "recovery-report.md").read_text()
        assert "INCONCLUSIVE" in recovery and "—" in recovery
