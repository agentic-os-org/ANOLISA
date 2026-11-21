"""Cumulative counter reset regressions for drop summaries.

counter_delta tolerated counter resets by clamping the negative delta to
zero, which also dropped the first sample of the new epoch:
counter_delta([10, 13, 2, 5]) returned 6 instead of 8, understating
ring/channel drops and completed events in benchmark summaries. The
post-reset sample must count while the first-sample baseline and
ordinary deltas stay unchanged.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

BENCHMARK_DIR = Path(__file__).parents[1]
SINGLE_RUN_DIR = BENCHMARK_DIR / "single_run"
sys.path.insert(0, str(SINGLE_RUN_DIR))

import plot_metrics  # noqa: E402
import render_report  # noqa: E402


def test_reset_epoch_sample_is_counted() -> None:
    assert render_report.counter_delta([10, 13, 2, 5]) == 8


def test_reset_to_zero_then_growth_is_counted() -> None:
    assert render_report.counter_delta([10, 0, 4]) == 4


def test_multiple_resets_count_every_epoch_sample() -> None:
    # epochs: (2->5)=3, reset sample 1 + (1->4)=3, reset sample 0 + (0->2)=2
    assert render_report.counter_delta([2, 5, 1, 4, 0, 2]) == 9


def test_first_sample_stays_the_baseline() -> None:
    assert render_report.counter_delta([]) == 0
    assert render_report.counter_delta([10]) == 0


def test_ordinary_deltas_unchanged() -> None:
    assert render_report.counter_delta([5, 7, 9]) == 4
    assert render_report.counter_delta([0, 3, 3]) == 3


def test_summarize_drops_counts_events_after_a_reset(tmp_path: Path) -> None:
    metrics = tmp_path / "agentsight.metrics"
    metrics.write_text(
        "ring_buffer_dropped,channel_dropped,completed\n"
        "10,0,100\n"
        "13,0,105\n"
        "2,0,110\n"
        "5,0,120\n",
        encoding="utf-8",
    )
    summary = render_report.summarize_drops(metrics, load_requests=200)
    assert summary["ring_buffer_dropped"] == 8
    assert summary["completed"] == 20
    assert summary["total_dropped"] == 8
    assert summary["drop_rate"] == 8 / (20 + 8) * 100


def test_html_report_counts_drops_and_evictions_across_resets(tmp_path: Path) -> None:
    metrics = tmp_path / "metrics.csv"
    metrics.write_text(
        "monotonic_timestamp,process_alive,cpu_pct,rss_mb,"
        "ring_buffer_dropped,channel_dropped,completed,eviction_count\n"
        "10,1,2,20,10,0,100,8\n"
        "11,1,2,20,13,0,105,9\n"
        "12,1,2,20,2,0,110,0\n"
        "13,1,2,20,5,0,120,2\n",
        encoding="utf-8",
    )
    output = tmp_path / "metrics-report.html"
    subprocess.run(
        [
            sys.executable,
            str(SINGLE_RUN_DIR / "plot_metrics.py"),
            str(metrics),
            "--output",
            str(output),
        ],
        check=True,
        capture_output=True,
    )
    report = output.read_text(encoding="utf-8")
    markdown = render_report.summarize_drops(metrics, load_requests=200)
    assert markdown["total_dropped"] == 8
    assert '<div class="label">丢弃次数</div><div class="value">8</div>' in report
    assert '<div class="label">淘汰次数</div><div class="value">3</div>' in report
    assert 'data-chart="chart-loss"' in report


def test_html_counter_delta_preserves_missing_and_single_sample_results() -> None:
    assert plot_metrics.counter_delta([None, None]) is None
    assert plot_metrics.counter_delta([None, 7, None]) == 0
    assert plot_metrics.counter_delta([3, None, 5]) == 2
