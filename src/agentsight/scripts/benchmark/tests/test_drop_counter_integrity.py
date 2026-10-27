"""Incomplete cumulative counters must not manufacture a zero-loss report."""

from __future__ import annotations

import csv
import gzip
from pathlib import Path

import pytest
from test_campaign_evidence import render_report, summary, thresholds
from test_campaign_measurements import campaign_evaluation


def csv_metrics(path: Path, records: list[dict[str, object]], compressed: bool) -> Path:
    if compressed:
        path = path.with_suffix(".csv.gz")
    opener = gzip.open if compressed else Path.open
    with opener(path, "wt", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(records[0]))
        writer.writeheader()
        writer.writerows(records)
    return path


def samples() -> list[dict[str, object]]:
    return [
        {"timestamp": i, "ring_buffer_dropped": 0, "channel_dropped": 0, "completed": i * 10}
        for i in range(3)
    ]


@pytest.mark.parametrize("compressed", [False, True])
@pytest.mark.parametrize("field", ["ring_buffer_dropped", "channel_dropped"])
@pytest.mark.parametrize("bad", ["", "NaN", "invalid", -1, 0.5])
def test_gap_cannot_be_erased_between_zero_samples(
    tmp_path: Path, compressed: bool, field: str, bad: object
) -> None:
    records = samples()
    records[1][field] = bad
    path = csv_metrics(tmp_path / "metrics.csv", records, compressed)
    drops = render_report.summarize_drops(path, 20)
    assert drops[field] is None
    assert drops["total_dropped"] is None
    assert drops["drop_rate"] is None
    measured = summary()
    measured["drop_rate"] = drops["drop_rate"]
    result = campaign_evaluation.evaluate(measured, thresholds())
    assert result["verdict"] == "INCONCLUSIVE"
    assert result["missing"] == ["drop_rate"]


def test_one_absent_counter_cannot_prove_total_loss(tmp_path: Path) -> None:
    records = samples()
    for record in records:
        del record["channel_dropped"]
    drops = render_report.summarize_drops(csv_metrics(tmp_path / "metrics.csv", records, False), 20)
    assert drops["ring_buffer_dropped"] == 0
    assert drops["channel_dropped"] is None
    assert drops["drop_rate"] is None


def test_valid_resets_count_new_epoch_without_counting_initial_baseline(tmp_path: Path) -> None:
    records = samples()
    for record, ring, channel in zip(records, [100, 104, 2], [50, 53, 1]):
        record.update(ring_buffer_dropped=ring, channel_dropped=channel)
    drops = render_report.summarize_drops(csv_metrics(tmp_path / "metrics.csv", records, False), 20)
    assert drops["ring_buffer_dropped"] == 6
    assert drops["channel_dropped"] == 4
    assert drops["total_dropped"] == 10
    assert drops["completed"] == 20
    assert drops["drop_rate"] == pytest.approx(100 / 3)


def test_invalid_completed_counter_uses_request_denominator(tmp_path: Path) -> None:
    records = samples()
    records[1]["completed"] = -100
    records[-1]["ring_buffer_dropped"] = 2
    drops = render_report.summarize_drops(csv_metrics(tmp_path / "metrics.csv", records, False), 20)
    assert drops["completed"] is None
    assert drops["drop_rate"] == 10


def test_capture_reconciliation_remains_independent_loss_evidence(tmp_path: Path) -> None:
    records = samples()
    records[1]["ring_buffer_dropped"] = ""
    drops = render_report.summarize_drops(csv_metrics(tmp_path / "metrics.csv", records, False), 20)
    result = render_report.build_summary(
        protocol="http1",
        qps="10",
        duration="2",
        load={"requests": 20, "http_success": 20, "latency": {"p99": 1}},
        resources={"process_alive": {"min": 1}},
        drops=drops,
        validation={"completeness_ratio": 0.9, "token_accuracy": 1},
    )
    assert result["drop_rate_source"] == "capture_reconciliation"
    assert result["drop_rate"] == pytest.approx(0.1)
