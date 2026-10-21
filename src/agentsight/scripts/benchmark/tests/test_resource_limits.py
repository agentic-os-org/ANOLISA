"""Verify the CI resource gate fails closed and accepts exact limit boundaries."""

from __future__ import annotations

import csv
import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any

import pytest

SINGLE_RUN = Path(__file__).parents[1] / "single_run"
sys.path.insert(0, str(SINGLE_RUN))
import check_resource_limits
from test_resource_report import artifacts, resource_report


@pytest.fixture
def summary() -> dict[str, Any]:
    return {
        "valid": True,
        "rss_unit": "MiB",
        "cpu_unit": "% of one logical CPU (100% = one logical CPU)",
        "settings": {
            "agents": 32,
            "connection_capacity": 128,
            "payload_kb": 1024,
            "tokens_per_second": 100,
            "tokens_per_response": 1000,
            "duration": 300,
            "ring_buffer_mb": 64,
            "cpu_budget_percent": 25,
            "mode": "cli",
        },
        "runs": {
            "run": {
                "valid": True,
                "measurement_window": {"start": 10, "end": 310},
                "rss_average_mib": 265,
                "rss_peak_mib": 300,
                "cpu_average_pct": 23.5,
                "cpu_p95_pct": 34,
                "cpu_peak_pct": 42,
            }
        },
    }


def test_exact_limits_pass(summary: dict[str, Any]) -> None:
    passed, text = check_resource_limits.evaluate(summary)
    assert passed
    assert text.count("| PASS |") == 5
    assert "265 MiB" in text
    assert "23.5 %" in text
    assert "Result: PASS" in text


@pytest.mark.parametrize(
    ("metric", "value"),
    [
        ("rss_average_mib", 265.000001),
        ("rss_peak_mib", 300.000001),
        ("cpu_average_pct", 23.500001),
        ("cpu_p95_pct", 34.000001),
        ("cpu_peak_pct", 42.000001),
    ],
)
def test_each_limit_fails_without_rounding(
    summary: dict[str, Any], metric: str, value: float
) -> None:
    summary["runs"]["run"][metric] = value
    passed, text = check_resource_limits.evaluate(summary)
    assert not passed
    assert "exceeds its limit" in text
    assert text.count("| FAIL |") == 1


@pytest.mark.parametrize("metric", list(check_resource_limits.LIMITS))
@pytest.mark.parametrize("value", [None, True, False, "0", [], {}, -1, float("nan"), float("inf")])
def test_invalid_metrics_fail(summary: dict[str, Any], metric: str, value: Any) -> None:
    summary["runs"]["run"][metric] = value
    passed, text = check_resource_limits.evaluate(summary)
    assert not passed
    assert f"Missing or invalid metric: runs.run.{metric}" in text


@pytest.mark.parametrize("metric", list(check_resource_limits.LIMITS))
def test_missing_metrics_fail(summary: dict[str, Any], metric: str) -> None:
    del summary["runs"]["run"][metric]
    assert not check_resource_limits.evaluate(summary)[0]


@pytest.mark.parametrize("scope", ["summary", "run"])
@pytest.mark.parametrize("value", [None, False, 1, "true"])
def test_invalid_measurements_fail_even_below_limits(
    summary: dict[str, Any], scope: str, value: Any
) -> None:
    target = summary if scope == "summary" else summary["runs"]["run"]
    target["valid"] = value
    summary["runs"]["run"]["cpu_average_pct"] = 1
    passed, text = check_resource_limits.evaluate(summary)
    assert not passed
    assert "Benchmark is INVALID" in text


@pytest.mark.parametrize("setting", list(check_resource_limits.EXPECTED_SETTINGS))
@pytest.mark.parametrize("replacement", [None, False, "unexpected"])
def test_changed_or_missing_load_settings_fail(
    summary: dict[str, Any], setting: str, replacement: Any
) -> None:
    if replacement is None:
        del summary["settings"][setting]
    else:
        summary["settings"][setting] = replacement
    passed, text = check_resource_limits.evaluate(summary)
    assert not passed
    assert f"Expected settings.{setting}" in text


@pytest.mark.parametrize("unit", ["rss_unit", "cpu_unit"])
def test_incompatible_units_fail(summary: dict[str, Any], unit: str) -> None:
    summary[unit] = "host percent"
    assert not check_resource_limits.evaluate(summary)[0]


@pytest.mark.parametrize("value", [None, [], {}, {"runs": {}}, {"runs": {"run": []}}])
def test_malformed_summary_is_rejected(value: Any) -> None:
    with pytest.raises(ValueError):
        check_resource_limits.evaluate(value)


def test_ab_summary_is_rejected(summary: dict[str, Any]) -> None:
    run = summary["runs"].pop("run")
    summary["runs"] = {"before": run, "after": run}
    with pytest.raises(ValueError, match="exactly one run"):
        check_resource_limits.evaluate(summary)


@pytest.mark.parametrize(
    "window",
    [
        None,
        [],
        {},
        {"start": True, "end": 301},
        {"start": 10, "end": 12},
        {"start": 10, "end": float("inf")},
        {"start": float("nan"), "end": 310},
    ],
)
def test_short_or_invalid_measurement_window_fails(summary: dict[str, Any], window: Any) -> None:
    summary["runs"]["run"]["measurement_window"] = window
    assert not check_resource_limits.evaluate(summary)[0]


def test_generated_resource_report_is_accepted(tmp_path: Path, summary: dict[str, Any]) -> None:
    directory = artifacts(tmp_path, "run", 23, 260)
    (directory / "agents/start.json").write_text(json.dumps({"start": 10, "end": 310}))
    (tmp_path / "settings.json").write_text(json.dumps(summary["settings"]))
    metrics = directory / "metrics.csv"
    with metrics.open(newline="") as handle:
        rows = list(csv.DictReader(handle))
    rows[-1].update(monotonic_timestamp="310", cpu_interval_seconds="299")
    with metrics.open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    assert resource_report.report(tmp_path, {"run": {}})
    generated = json.loads((tmp_path / "resource-summary.json").read_text())
    assert check_resource_limits.evaluate(generated)[0]


@pytest.mark.parametrize("case", ["pass", "exceeded", "missing", "malformed", "invalid"])
def test_cli_exit_status_and_job_summary(
    tmp_path: Path, summary: dict[str, Any], case: str
) -> None:
    report = tmp_path / "resource-summary.json"
    job_summary = tmp_path / "job-summary.md"
    job_summary.write_text("Existing summary\n", encoding="utf-8")
    if case == "exceeded":
        summary["runs"]["run"]["cpu_peak_pct"] = 43
    if case == "invalid":
        summary["valid"] = False
    if case != "missing":
        report.write_text("{" if case == "malformed" else json.dumps(summary), encoding="utf-8")
    result = subprocess.run(
        [
            sys.executable,
            "-B",
            str(SINGLE_RUN / "check_resource_limits.py"),
            "--summary",
            str(report),
        ],
        env={**os.environ, "GITHUB_STEP_SUMMARY": str(job_summary)},
        text=True,
        capture_output=True,
        check=False,
    )
    assert result.returncode == (0 if case == "pass" else 1)
    assert "Traceback" not in result.stderr
    assert f"Result: {'PASS' if case == 'pass' else 'FAIL'}" in result.stdout
    assert job_summary.read_text(encoding="utf-8") == "Existing summary\n" + result.stdout
