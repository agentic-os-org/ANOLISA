"""Soak trends use elapsed time even when the wall clock is adjusted."""

import csv
from pathlib import Path

import pytest
from test_benchmark import render_report


def metrics(tmp_path: Path, walls: list[float], clocks: list[object] | None) -> Path:
    path = tmp_path / "metrics.csv"
    fields = ["timestamp", "rss_mb", "file_descriptors", "threads", "active_connections"]
    if clocks is not None:
        fields.append("monotonic_timestamp")
    with path.open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=fields)
        writer.writeheader()
        for index, wall in enumerate(walls):
            row = dict(
                timestamp=wall,
                rss_mb=100 + index * 10,
                file_descriptors=10 + index,
                threads=2 + index,
                active_connections=5 + index,
            )
            if clocks is not None:
                row["monotonic_timestamp"] = clocks[index]
            writer.writerow(row)
    return path


@pytest.mark.parametrize("walls", [[1000, 4660, 8320], [1000, 940, 880], [1000, 1000, 1000]])
def test_clock_steps_do_not_hide_resource_growth(tmp_path: Path, walls: list[float]) -> None:
    summary = render_report.summarize_resources(metrics(tmp_path, walls, [10, 70, 130]))
    assert summary["rss_mb"]["slope_per_hour"] == pytest.approx(600)
    assert summary["rss_mb"]["rolling_5m_max_increase"] == 20
    for name in ("file_descriptors", "threads", "active_connections"):
        assert summary[name]["slope_per_hour"] == pytest.approx(60)


def test_legacy_wall_only_csv_keeps_its_trends(tmp_path: Path) -> None:
    summary = render_report.summarize_resources(metrics(tmp_path, [1000, 1060, 1120], None))
    assert summary["rss_mb"]["slope_per_hour"] == pytest.approx(600)


@pytest.mark.parametrize("clocks", [[10, "", 130], [10, "nan", 130], [10, 9, 130], [10, 10, 130]])
def test_broken_elapsed_time_cannot_prove_a_soak_gate(tmp_path: Path, clocks: list[object]) -> None:
    summary = render_report.summarize_resources(metrics(tmp_path, [1000, 1060, 1120], clocks))
    assert summary["rss_mb"]["slope_per_hour"] is None
    assert summary["rss_mb"]["rolling_5m_max_increase"] is None
    assert summary["rss_mb"]["max"] == 120


def test_real_growth_fails_the_frozen_soak_threshold(tmp_path: Path) -> None:
    from test_benchmark import campaign_data, campaign_evaluation, complete_summary

    summary = complete_summary()
    summary["resources"] = render_report.summarize_resources(
        metrics(tmp_path, [1000, 4660, 8320], [10, 70, 130])
    )
    outcome = campaign_evaluation.evaluate(summary, campaign_data(tmp_path)["thresholds"], "soak")
    assert "rss_slope_mb_per_hour" in outcome["failed"]
