"""Only observations inside the recovery phase may establish recovery."""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from test_campaign_evidence import aggregate_report, campaign_evidence, thresholds
from test_recovery_evidence_gaps import recovery_fixture, rows


@pytest.mark.parametrize("compressed", [False, True])
@pytest.mark.parametrize("end", [1, 2, 3])
def test_samples_after_phase_end_cannot_complete_a_window(
    tmp_path: Path, compressed: bool, end: int
) -> None:
    phases = recovery_fixture(tmp_path, rows(8), compressed)
    phases["recover"][1]["ended_at_unix"] = end
    result = campaign_evidence.recovery_outcome(
        phases, {"tolerance_ratio": 0.1, "recovery_window_seconds": 4}, thresholds()
    )
    assert result["verdict"] == "INCONCLUSIVE"
    assert set(result["missing"]) == {
        "effective_qps",
        "latency_p99_ms",
        "rss_mb",
        "channel_length",
        "connection_cache_bytes",
    }
    assert all(value is None for value in result["seconds"].values())


@pytest.mark.parametrize("invalid", [None, True, "nan", -1, 0])
def test_invalid_declared_end_is_not_ignored(tmp_path: Path, invalid: object) -> None:
    phases = recovery_fixture(tmp_path, rows(8))
    phases["recover"][1]["ended_at_unix"] = invalid
    result = campaign_evidence.recovery_outcome(
        phases, {"tolerance_ratio": 0.1, "recovery_window_seconds": 4}, thresholds()
    )
    assert result["verdict"] == "INCONCLUSIVE"
    assert result["missing"] == ["recover.ended_at_unix"]


def test_legacy_artifacts_without_end_keep_observed_window(tmp_path: Path) -> None:
    phases = recovery_fixture(tmp_path, rows(8))
    result = campaign_evidence.recovery_outcome(
        phases, {"tolerance_ratio": 0.1, "recovery_window_seconds": 4}, thresholds()
    )
    assert result["verdict"] == "PASS"
    assert set(result["seconds"].values()) == {0}


def test_pre_phase_metrics_do_not_establish_recovery() -> None:
    samples = [(second, 100) for second in range(7)]
    result = campaign_evidence.continuous_recovery(
        samples, 100, 0.1, 4, higher_is_better=False, started_at=3
    )
    assert result is None


def test_raw_load_records_are_filtered_before_second_bucketing(tmp_path: Path) -> None:
    path = tmp_path / "recover/run-result.json"
    measurement = path.parent / "measurement"
    measurement.mkdir(parents=True)
    points = []
    for time, requests, latency in [
        (9.9, 1000, 999),
        (10.1, 1000, 999),
        (10.6, 2, 10),
        (11, 2, 10),
        (12, 2, 10),
        (12.1, 1000, 999),
    ]:
        points.extend(
            [
                {"metric": "benchmark_requests", "data": {"time": time, "value": requests}},
                {"metric": "benchmark_latency", "data": {"time": time, "value": latency}},
            ]
        )
    (measurement / "k6.jsonl").write_text("\n".join(map(json.dumps, points)) + "\n")
    result = campaign_evidence.load_samples(path, started_at=10.5, ended_at=12)
    assert result["effective_qps"] == [(10.5, 2), (11, 2), (12, 2)]
    assert result["latency_p99_ms"] == [(10.5, 10), (11, 10), (12, 10)]


def test_report_retains_unproven_recovery_and_valid_boundary_samples(tmp_path: Path) -> None:
    phases = recovery_fixture(tmp_path, rows(8))
    phases["recover"][1]["ended_at_unix"] = 3
    result = aggregate_report.write_recovery(
        tmp_path,
        list(phases.values()),
        {"tolerance_ratio": 0.1, "recovery_window_seconds": 4},
        thresholds(),
    )
    assert result[("baseline", 1)]["verdict"] == "INCONCLUSIVE"
    assert "INCONCLUSIVE" in (tmp_path / "recovery-report.md").read_text()
    phases["recover"][1]["ended_at_unix"] = 4
    outcome = campaign_evidence.recovery_outcome(
        phases, {"tolerance_ratio": 0.1, "recovery_window_seconds": 4}, thresholds()
    )
    assert outcome["verdict"] == "PASS"
