"""Recovery must be sustained through real observations, not erased bad samples."""

from __future__ import annotations

import csv
import gzip
import json
from pathlib import Path
from typing import Any

import pytest
from test_campaign_evidence import aggregate_report, campaign_evidence, summary, thresholds


def recovery_fixture(
    root: Path, samples: list[dict[str, Any]], compressed: bool = False
) -> dict[str, tuple[Path, dict[str, Any]]]:
    phases = {}
    for label in campaign_evidence.RECOVERY_PHASES:
        path = root / label / "run-result.json"
        path.parent.mkdir(parents=True)
        run = {
            "scenario": "recovery",
            "version": "baseline",
            "repetition": 1,
            "label": label,
            "summary": summary(),
            "evaluation": {"verdict": "PASS"},
            "started_at_unix": 0,
        }
        path.write_text(json.dumps(run))
        phases[label] = (path, run)
    measurement = phases["recover"][0].parent / "measurement"
    measurement.mkdir()
    points = [
        {"metric": metric, "data": {"time": second, "value": value}}
        for second in range(12)
        for metric, value in (("benchmark_requests", 2), ("benchmark_latency", 10))
    ]
    (measurement / "k6.jsonl").write_text("\n".join(map(json.dumps, points)) + "\n")
    metrics = measurement / ("metrics.csv.gz" if compressed else "metrics.csv")
    opener = gzip.open if compressed else Path.open
    with opener(metrics, "wt", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(samples[0]))
        writer.writeheader()
        writer.writerows(samples)
    return phases


def rows(end: int = 4) -> list[dict[str, Any]]:
    return [
        {"timestamp": second, "rss_mb": 100, "channel_length": 2, "connection_cache_bytes": 20}
        for second in range(end + 1)
    ]


@pytest.mark.parametrize("compressed", [False, True])
@pytest.mark.parametrize("field", ["rss_mb", "channel_length", "connection_cache_bytes"])
@pytest.mark.parametrize("invalid", ["", "NaN", "broken"])
def test_invalid_resource_sample_breaks_continuous_recovery(
    tmp_path: Path, compressed: bool, field: str, invalid: str
) -> None:
    samples = rows()
    samples[2][field] = invalid
    phases = recovery_fixture(tmp_path, samples, compressed)
    outcome = campaign_evidence.recovery_outcome(
        phases, {"tolerance_ratio": 0.1, "recovery_window_seconds": 4}, thresholds()
    )
    assert outcome["verdict"] == "INCONCLUSIVE"
    assert outcome["seconds"][field] is None
    assert field in outcome["missing"]
    unaffected = {"rss_mb", "channel_length", "connection_cache_bytes"} - {field}
    assert all(outcome["seconds"][key] == 0 for key in unaffected)


def test_later_complete_window_recovers_after_the_gap(tmp_path: Path) -> None:
    samples = rows(7)
    samples[2]["rss_mb"] = ""
    phases = recovery_fixture(tmp_path, samples)
    outcome = campaign_evidence.recovery_outcome(
        phases, {"tolerance_ratio": 0.1, "recovery_window_seconds": 4}, thresholds()
    )
    assert outcome["verdict"] == "PASS"
    assert outcome["seconds"]["rss_mb"] == 3
    assert outcome["seconds"]["effective_qps"] == 0


@pytest.mark.parametrize("bad_time", ["", "nan", "invalid"])
def test_unknown_sample_time_cannot_prove_an_uninterrupted_window(
    tmp_path: Path, bad_time: str
) -> None:
    samples = rows()
    samples[2]["timestamp"] = bad_time
    phases = recovery_fixture(tmp_path, samples)
    outcome = campaign_evidence.recovery_outcome(
        phases, {"tolerance_ratio": 0.1, "recovery_window_seconds": 4}, thresholds()
    )
    assert outcome["verdict"] == "INCONCLUSIVE"
    assert "rss_mb" in outcome["missing"]


def test_report_names_unproven_metrics_and_keeps_other_measurements(tmp_path: Path) -> None:
    samples = rows()
    samples[2]["rss_mb"] = ""
    phases = recovery_fixture(tmp_path, samples)
    outcomes = aggregate_report.write_recovery(
        tmp_path,
        list(phases.values()),
        {"tolerance_ratio": 0.1, "recovery_window_seconds": 4},
        thresholds(),
    )
    assert outcomes[("baseline", 1)]["verdict"] == "INCONCLUSIVE"
    text = (tmp_path / "recovery-report.md").read_text()
    assert "rss_mb" in text and "INCONCLUSIVE" in text
    assert "0.00s/0.00s/—" in text


def test_optional_metrics_unavailable_in_both_phases_remain_optional(tmp_path: Path) -> None:
    samples = [{"timestamp": second, "rss_mb": 100} for second in range(5)]
    phases = recovery_fixture(tmp_path, samples)
    for _, run in phases.values():
        for field in ("channel_length", "connection_cache_bytes"):
            run["summary"]["resources"].pop(field)
    outcome = campaign_evidence.recovery_outcome(
        phases, {"tolerance_ratio": 0.1, "recovery_window_seconds": 4}, thresholds()
    )
    assert outcome["verdict"] == "PASS"
    assert set(outcome["seconds"]) == {"effective_qps", "latency_p99_ms", "rss_mb"}


def test_point_sample_api_preserves_existing_filtering_contract(tmp_path: Path) -> None:
    samples = rows()
    samples[2]["rss_mb"] = ""
    phases = recovery_fixture(tmp_path, samples)
    assert campaign_evidence.resource_samples(phases["recover"][0], "rss_mb") == [
        (0, 100),
        (1, 100),
        (3, 100),
        (4, 100),
    ]


@pytest.mark.parametrize("timestamp", [1, 0])
def test_repeated_or_reversed_resource_clock_is_not_sorted_into_a_pass(
    tmp_path: Path, timestamp: int
) -> None:
    samples = rows()
    samples[2]["timestamp"] = timestamp
    phases = recovery_fixture(tmp_path, samples)
    outcome = campaign_evidence.recovery_outcome(
        phases, {"tolerance_ratio": 0.1, "recovery_window_seconds": 4}, thresholds()
    )
    assert outcome["verdict"] == "INCONCLUSIVE"
    assert "rss_mb" in outcome["missing"]
