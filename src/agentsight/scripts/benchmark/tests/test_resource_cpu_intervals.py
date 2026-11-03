"""Resource improvement claims require unique, ordered CPU interval evidence."""

import csv
import json
from pathlib import Path

import pytest
from test_resource_report import artifacts, resource_report, row


@pytest.mark.parametrize(
    "samples",
    [
        [row(11, 1, 20, 250), row(11, 1, 20, 250)],
        [row(12, 1, 20, 250), row(11, 1, 20, 250)],
        [row(11.5, 1.5, 20, 250), row(12, 1, 80, 250)],
        [row(12, 2, 20, 250), row(11.5, 1, 80, 250)],
    ],
)
def test_overlapping_or_reversed_cpu_samples_are_invalid(samples: list[dict[str, str]]) -> None:
    measured = resource_report.resource_stats(samples, 10, 12)
    assert not measured["cpu_samples_valid"]
    assert measured["cpu_measurement_seconds"] <= 2


def test_duplicate_intervals_cannot_hide_a_measurement_gap(tmp_path: Path) -> None:
    directory = artifacts(tmp_path, "run", 20, 250)
    samples = [row(10, "", "", 250), row(11, 1, 20, 250), row(11, 1, 20, 250)]
    with (directory / "metrics.csv").open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(samples[0]))
        writer.writeheader()
        writer.writerows(samples)
    measured = resource_report.summarize(directory, {})
    assert not measured["valid"]
    assert measured["resource_error"]
    assert measured["cpu_measurement_seconds"] == 1


def test_overlap_suppresses_text_and_json_ab_improvements(tmp_path: Path) -> None:
    artifacts(tmp_path, "before", 80, 400)
    directory = artifacts(tmp_path, "after", 20, 250)
    path = directory / "metrics.csv"
    lines = path.read_text().splitlines()
    path.write_text("\n".join([lines[0], lines[1], lines[2], lines[2]]) + "\n")
    assert not resource_report.report(tmp_path, {"before": {}, "after": {}})
    summary = json.loads((tmp_path / "resource-summary.json").read_text())
    assert summary["runs"]["before"]["valid"]
    assert not summary["runs"]["after"]["valid"]
    assert not summary["comparison"]
    assert "INVALID" in (tmp_path / "resource-report.txt").read_text()


def test_adjacent_multicore_intervals_keep_time_weighting() -> None:
    samples = [row(10, "", "", 250), row(11, 1, 150, 250), row(13, 2, 30, 250)]
    measured = resource_report.resource_stats(samples, 10, 13)
    assert measured["cpu_samples_valid"]
    assert measured["cpu_average_pct"] == pytest.approx(70)
    assert measured["cpu_seconds"] == pytest.approx(2.1)
    assert measured["cpu_measurement_seconds"] == 3


def test_submicrosecond_rounding_is_clipped_without_double_counting() -> None:
    samples = [row(11, 1, 20, 250), row(12, 1.0000005, 20, 250)]
    measured = resource_report.resource_stats(samples, 10, 12)
    assert measured["cpu_samples_valid"]
    assert measured["cpu_measurement_seconds"] == 2


def test_duplicate_tail_does_not_invalidate_unrelated_load_window() -> None:
    samples = [row(11, 1, 20, 250), row(12, 1, 30, 250), row(14, 1, 80, 250), row(14, 1, 80, 250)]
    measured = resource_report.resource_stats(samples, 10, 12)
    assert measured["cpu_samples_valid"]
    assert measured["cpu_measurement_seconds"] == 2
    assert measured["cpu_average_pct"] == 25
    assert not resource_report.resource_stats(samples, 12, float("inf"))["cpu_samples_valid"]


def test_boundary_interval_still_splits_between_load_and_tail() -> None:
    samples = [row(13, 2, 150, 250)]
    load = resource_report.resource_stats(samples, 10, 12)
    tail = resource_report.resource_stats(samples, 12, float("inf"))
    assert load["cpu_samples_valid"] and tail["cpu_samples_valid"]
    assert load["cpu_seconds"] == tail["cpu_seconds"] == 1.5
