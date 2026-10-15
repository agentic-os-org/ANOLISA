from pathlib import Path
import sys
import pytest

ROOT = Path(__file__).resolve().parents[5]
BENCH = ROOT / "src/agentsight/scripts/benchmark"
sys.path.insert(0, str(BENCH / "single_run"))
sys.path.insert(0, str(BENCH / "campaign"))

import csv
import json
import subprocess
import aggregate_report as aggregate


def run_record(scenario="smoke"):
    return {
        "scenario": scenario,
        "version": "baseline",
        "label": "sample, 标签",
        "repetition": 1,
        "qps": 20,
        "duration_seconds": 2,
        "harness_exit_code": 0,
        "evaluation": {
            "verdict": "INCONCLUSIVE",
            "missing": ["trace", "a,b"],
            "failed": [],
        },
    }


def read_inventory(root):
    with (root / "run-inventory.csv").open(encoding="utf-8", newline="") as stream:
        return list(csv.DictReader(stream))


def test_inventory_preserves_all_scenarios_and_source_paths(tmp_path):
    items = []
    for scenario in ["smoke", "capacity", "matrix", "soak", "recovery", "fault"]:
        path = tmp_path / "runs" / scenario / "run-result.json"
        path.parent.mkdir(parents=True)
        record = run_record(scenario)
        path.write_text(json.dumps(record), encoding="utf-8")
        items.append((path, record))
    aggregate.write_run_inventory(tmp_path, items)
    rows = read_inventory(tmp_path)
    assert {row["scenario"] for row in rows} == {item[1]["scenario"] for item in items}
    assert all(row["label"] == "sample, 标签" for row in rows)
    assert all(json.loads(row["missing_gates"]) == ["trace", "a,b"] for row in rows)
    assert all(row["result_path"].startswith("runs/") for row in rows)
    assert all(
        path.read_text(encoding="utf-8") == json.dumps(record) for path, record in items
    )


def test_inventory_is_deterministic_and_retains_missing_fields(tmp_path):
    items = [
        (tmp_path / "runs/z/run-result.json", {"scenario": "fault"}),
        (tmp_path / "runs/a/run-result.json", run_record()),
    ]
    aggregate.write_run_inventory(tmp_path, items)
    first = (tmp_path / "run-inventory.csv").read_bytes()
    aggregate.write_run_inventory(tmp_path, list(reversed(items)))
    assert (tmp_path / "run-inventory.csv").read_bytes() == first
    rows = read_inventory(tmp_path)
    assert rows[0]["result_path"] == "runs/a/run-result.json"
    assert rows[1]["verdict"] == ""
    assert rows[1]["harness_exit_code"] == ""


def test_empty_inventory_has_header_without_invented_runs(tmp_path):
    aggregate.write_run_inventory(tmp_path, [])
    assert read_inventory(tmp_path) == []
    assert (
        (tmp_path / "run-inventory.csv")
        .read_text(encoding="utf-8")
        .startswith("scenario,")
    )


def test_actual_aggregate_cli_emits_inventory(tmp_path):
    (tmp_path / "manifest.json").write_text("{}", encoding="utf-8")
    path = tmp_path / "runs/smoke/baseline/qps-20/rep-1/run-result.json"
    path.parent.mkdir(parents=True)
    path.write_text(json.dumps(run_record()), encoding="utf-8")
    result = subprocess.run(
        [
            sys.executable,
            str(BENCH / "campaign/aggregate_report.py"),
            "--campaign",
            str(BENCH / "campaign/campaign.example.json"),
            "--results",
            str(tmp_path),
        ],
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr
    assert len(read_inventory(tmp_path)) == 1
    assert (tmp_path / "final-summary.json").is_file()
