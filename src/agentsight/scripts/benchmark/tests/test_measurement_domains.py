"""Physical measurement domains must hold across campaign quality gates."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
from test_campaign_evidence import campaign_evidence, summary, thresholds, write_recovery_artifacts
from test_campaign_measurements import campaign_evaluation
from test_recovery_evidence_gaps import recovery_fixture, rows


@pytest.mark.parametrize(
    "field,keys",
    [
        ("http_success_rate", ("http_success_rate",)),
        ("trace_completeness", ("trace_completeness",)),
        ("token_accuracy", ("token_accuracy",)),
        ("drop_rate", ("drop_rate",)),
        ("latency_p99_ms", ("latency_ms", "p99")),
        ("rss_max_mb", ("resources", "rss_mb", "max")),
        ("throughput_ratio", ("effective_qps",)),
    ],
)
@pytest.mark.parametrize("value", [-1, -0.001])
def test_negative_measurements_cannot_be_pass_or_fail_evidence(
    field: str, keys: tuple[str, ...], value: float
) -> None:
    measured = summary()
    leaf = measured
    for key in keys[:-1]:
        leaf = leaf[key]
    leaf[keys[-1]] = value
    result = campaign_evaluation.evaluate(measured, thresholds())
    assert result["verdict"] == "INCONCLUSIVE"
    assert result["missing"] == [field]
    assert field not in result["failed"]


@pytest.mark.parametrize(
    "field", ["http_success_rate", "trace_completeness", "token_accuracy", "drop_rate"]
)
@pytest.mark.parametrize("value", [1.00001, 100])
def test_ratios_above_one_are_unusable(field: str, value: float) -> None:
    measured = summary()
    measured[field] = value
    result = campaign_evaluation.evaluate(measured, thresholds())
    assert result["verdict"] == "INCONCLUSIVE"
    assert result["missing"] == [field]


def test_zero_measurements_negative_slopes_and_throughput_overshoot_remain_valid() -> None:
    measured = summary()
    measured["effective_qps"] = 3
    measured["latency_ms"]["p99"] = 0
    measured["resources"]["rss_mb"].update(max=0, slope_per_hour=-1)
    for field in ("file_descriptors", "threads", "active_connections"):
        measured["resources"][field] = {"slope_per_hour": -1}
    assert campaign_evaluation.evaluate(measured, thresholds(), "soak")["verdict"] == "PASS"
    measured["effective_qps"] = 0
    result = campaign_evaluation.evaluate(measured, thresholds())
    assert result["verdict"] == "FAIL"
    assert result["failed"] == ["throughput_ratio"]
    assert result["missing"] == []


@pytest.mark.parametrize("field", ["http_success_rate", "trace_completeness", "token_accuracy"])
@pytest.mark.parametrize("value", [-0.1, 1.1])
def test_fault_report_preserves_invalid_quality_as_missing(
    tmp_path: Path, field: str, value: float
) -> None:
    path = tmp_path / "run-result.json"
    measurement = tmp_path / "measurement"
    measurement.mkdir()
    (measurement / "fault-results.json").write_text(
        json.dumps(
            {
                "outcomes": {case: {"handled": 1} for case in campaign_evidence.FAULT_CASES},
                "server_healthy_after": True,
                "process_alive_before": True,
                "process_alive_after": True,
                "sse_truncation_verified": True,
            }
        )
    )
    measured = summary()
    measured[field] = value
    outcome = campaign_evidence.fault_outcome(
        path, {"summary": measured}, {"repetitions_per_case": 1}, thresholds()
    )
    assert outcome["verdict"] == "INCONCLUSIVE"
    assert outcome["missing"] == [field]
    assert outcome["failed"] == []


@pytest.mark.parametrize("value", [-0.1, 1.1])
def test_recovery_quality_cannot_override_invalid_completeness(
    tmp_path: Path, value: float
) -> None:
    phases: dict[str, tuple[Path, dict[str, Any]]] = {}
    for label in campaign_evidence.RECOVERY_PHASES:
        path = tmp_path / label / "run-result.json"
        phases[label] = (
            path,
            {"summary": summary(), "started_at_unix": 0, "evaluation": {"verdict": "PASS"}},
        )
    write_recovery_artifacts(phases["recover"][0])
    phases["recover"][1]["summary"]["trace_completeness"] = value
    result = campaign_evidence.recovery_outcome(
        phases, {"tolerance_ratio": 0.1, "recovery_window_seconds": 2}, thresholds()
    )
    assert result["verdict"] == "INCONCLUSIVE"
    assert result["missing"] == ["trace_completeness"]
    assert set(result["seconds"].values()) == {1.0}


@pytest.mark.parametrize("field", ["rss_mb", "channel_length", "connection_cache_bytes"])
def test_negative_raw_resource_sample_breaks_recovery_window(tmp_path: Path, field: str) -> None:
    samples = rows()
    samples[2][field] = -1
    phases = recovery_fixture(tmp_path, samples)
    result = campaign_evidence.recovery_outcome(
        phases, {"tolerance_ratio": 0.1, "recovery_window_seconds": 4}, thresholds()
    )
    assert result["verdict"] == "INCONCLUSIVE"
    assert result["seconds"][field] is None
    assert result["missing"] == [field]


def test_negative_recovery_reference_is_not_a_physical_baseline() -> None:
    assert (
        campaign_evidence.continuous_recovery(
            [(0, 100), (1, 100), (2, 100)], -100, 0.1, 2, higher_is_better=True
        )
        is None
    )
