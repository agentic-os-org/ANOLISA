"""Verify CPU units, interval weighting, phase boundaries and A/B validity."""

from __future__ import annotations

import csv
import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parents[1] / "single_run"))

import collect_metrics
import resource_report


def row(timestamp: float, interval: float | str, cpu: float | str, rss: float) -> dict[str, str]:
    return {
        "monotonic_timestamp": str(timestamp),
        "cpu_interval_seconds": str(interval),
        "cpu_pct": str(cpu),
        "rss_mb": str(rss),
        "process_alive": "1",
    }


def test_cpu_uses_actual_elapsed_time_and_allows_multiple_cores(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(collect_metrics, "read_status", lambda _: {"VmRSS": 256000})
    monkeypatch.setattr(collect_metrics, "read_cpu_ticks", lambda _: 400)
    monkeypatch.setattr(collect_metrics.time, "monotonic", lambda: 12.0)
    monkeypatch.setattr(collect_metrics.os, "sysconf", lambda _: 100)
    measured, ticks, timestamp = collect_metrics.sample(123, 100, 10.0, 0, None)
    assert float(measured["cpu_pct"]) == 150
    assert measured["rss_mb"] == "250.000"
    assert measured["cpu_interval_seconds"] == 2
    assert measured["monotonic_timestamp"] == timestamp == 12
    assert ticks == 400


def test_load_and_drain_statistics_use_separate_windows_and_weight_cpu() -> None:
    rows = [row(9, 1, 999, 999), row(10, "", "", 200), row(11, 1, 30, 240)]
    rows += [row(14, 3, 100, 260), row(15, 1, 0, 900)]
    load = resource_report.resource_stats(rows, 10, 14)
    assert load["rss_average_mib"] == 220
    assert load["rss_peak_mib"] == 240
    assert load["cpu_average_pct"] == pytest.approx(82.5)
    assert load["cpu_seconds"] == pytest.approx(3.3)
    assert load["cpu_measurement_seconds"] == 4
    assert load["cpu_p95_pct"] == load["cpu_peak_pct"] == 100
    tail = resource_report.resource_stats(rows, 14, float("inf"))
    assert tail["rss_peak_mib"] == 900
    assert tail["cpu_average_pct"] == tail["cpu_peak_pct"] == 0
    assert tail["cpu_measurement_seconds"] == 1


def test_boundary_intervals_are_split_by_overlap() -> None:
    rows = [row(13, 2, 150, 250)]
    load = resource_report.resource_stats(rows, 10, 12)
    tail = resource_report.resource_stats(rows, 12, float("inf"))
    assert load["cpu_seconds"] == tail["cpu_seconds"] == 1.5
    assert load["cpu_average_pct"] == tail["cpu_average_pct"] == 150


def test_invalid_cpu_values_are_rejected_and_unrelated_intervals_are_ignored() -> None:
    valid = row(11, 1, 25, 250)
    for invalid in [row(float("nan"), 1, 25, 250), row(12, 1, "nan", 250), row(12, 1, -1, 250)]:
        stats = resource_report.resource_stats([valid, invalid], 10, 13)
        assert not stats["cpu_samples_valid"]
        assert stats["cpu_average_pct"] == 25
    stats = resource_report.resource_stats([row(9, 1, 999, 999), valid], 10, 12)
    assert stats["cpu_samples_valid"]
    assert stats["cpu_average_pct"] == stats["cpu_peak_pct"] == 25


@pytest.mark.parametrize("interval", ["", "nan", "0", "-1"])
def test_missing_or_invalid_interval_is_not_a_second_baseline(interval: str) -> None:
    rows = [row(10, "", "", 250), row(11, interval, "", 250), row(12, 1, 30, 250)]
    assert not resource_report.resource_stats(rows, 10, 12)["cpu_samples_valid"]


def artifacts(output: Path, label: str, cpu: float, rss: float) -> Path:
    directory = output / label
    (directory / "agents").mkdir(parents=True)
    records = {
        "worker-result.json": {
            "status": "complete",
            "returncode": 0,
            "trace_survived": True,
            "host_memory_mib": 1000,
            "logical_cpus": 8,
        },
        "agents/start.json": {"start": 10, "end": 12},
        "agent-summary.json": {"passed": True},
        "report.json": {"sent": 1, "completeness_ratio": 1, "token_accuracy": 1},
    }
    for name, record in records.items():
        (directory / name).write_text(json.dumps(record))
    rows = [row(10, "", "", rss), row(11, 1, cpu, rss), row(12, 1, cpu, rss)]
    with (directory / "metrics.csv").open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    return directory


def test_ab_reports_memory_reduction_and_cpu_regression(tmp_path: Path) -> None:
    artifacts(tmp_path, "before", 30, 400)
    artifacts(tmp_path, "after", 100, 250)
    assert resource_report.report(tmp_path, {"before": {}, "after": {}})
    summary = json.loads((tmp_path / "resource-summary.json").read_text())
    assert summary["comparison"]["rss_average_mib"] == {"mib": 150, "percent": 37.5}
    change = summary["comparison"]["cpu_average_pct"]
    assert change["percentage_points"] == -70
    assert change["percent"] == pytest.approx(-233.33333333333334)
    assert "100% = one logical CPU" in summary["cpu_unit"]
    report = (tmp_path / "resource-report.txt").read_text()
    assert "-70.00 pp" in report and "Average CPU" in report and "Peak CPU" in report


@pytest.mark.parametrize("cpu", ["", "nan", "inf", "-1", "broken"])
def test_invalid_cpu_suppresses_all_ab_improvement(tmp_path: Path, cpu: str) -> None:
    artifacts(tmp_path, "before", 30, 400)
    directory = artifacts(tmp_path, "after", 100, 250)
    path = directory / "metrics.csv"
    path.write_text(path.read_text().replace(",100,", f",{cpu},"))
    assert not resource_report.report(tmp_path, {"before": {}, "after": {}})
    summary = json.loads((tmp_path / "resource-summary.json").read_text())
    assert not summary["valid"] and not summary["comparison"]
    assert summary["runs"]["after"]["resource_error"]


def test_missing_window_and_partial_cpu_coverage_are_invalid(tmp_path: Path) -> None:
    directory = artifacts(tmp_path, "run", 0, 250)
    window = directory / "agents/start.json"
    window.write_text(json.dumps({"start": 10, "end": 20}))
    measured = resource_report.summarize(directory, {})
    assert not measured["valid"] and "95%" in measured["resource_error"]
    window.unlink()
    measured = resource_report.summarize(directory, {})
    assert not measured["valid"] and "window" in measured["resource_error"]


def test_zero_cpu_baseline_has_no_relative_percentage(tmp_path: Path) -> None:
    artifacts(tmp_path, "before", 0, 250)
    artifacts(tmp_path, "after", 150, 250)
    assert resource_report.report(tmp_path, {"before": {}, "after": {}})
    summary = json.loads((tmp_path / "resource-summary.json").read_text())
    assert summary["comparison"]["cpu_peak_pct"] == {"percentage_points": -150, "percent": None}
