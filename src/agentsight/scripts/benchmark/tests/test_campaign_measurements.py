"""Campaign verdict measurement-type regressions.

Booleans accepted as throughput/success/drop measurements and integer 1
accepted as a lifecycle boolean used to produce PASS/FAIL verdicts from
unusable evidence, and non-finite numeric measurements were compared
instead of being marked missing. These tests pin the INCONCLUSIVE
contract for unusable measurements while leaving frozen thresholds and
genuine pass/fail behavior unchanged.
"""

from __future__ import annotations

import math
import sys
from pathlib import Path
from typing import Any

BENCHMARK_DIR = Path(__file__).parents[1]
CAMPAIGN_DIR = BENCHMARK_DIR / "campaign"
sys.path.insert(0, str(CAMPAIGN_DIR))

import campaign_evaluation  # noqa: E402

THRESHOLDS: dict[str, float] = {
    "min_throughput_ratio": 0.9,
    "min_http_success_rate": 0.99,
    "min_trace_completeness": 0.999,
    "min_token_accuracy": 1.0,
    "max_p99_ms": 100,
    "max_drop_rate": 0.01,
    "max_rss_mb": 100,
    "max_rss_slope_mb_per_hour": 10,
    "max_fd_slope_per_hour": 1,
    "max_thread_slope_per_hour": 1,
    "max_socket_slope_per_hour": 1,
    "max_recovery_seconds": 10,
}


def complete_summary(**overrides: Any) -> dict[str, Any]:
    summary: dict[str, Any] = {
        "input_qps": 100.0,
        "effective_qps": 100.0,
        "http_success_rate": 1.0,
        "trace_completeness": 1.0,
        "token_accuracy": 1.0,
        "drop_rate": 0.0,
        "latency_ms": {"p99": 10.0},
        "resources": {
            "rss_mb": {"avg": 50.0, "p99": 55.0, "max": 60.0, "slope_per_hour": 1.0}
        },
        "process_survived": True,
        "runtime_clean": True,
    }
    summary.update(overrides)
    return summary


def evaluate(summary: dict[str, Any], scenario: str = "capacity") -> dict[str, Any]:
    return campaign_evaluation.evaluate(summary, THRESHOLDS, scenario)


def test_boolean_throughput_measurements_are_inconclusive() -> None:
    result = evaluate(complete_summary(input_qps=True, effective_qps=True))
    assert result["verdict"] == "INCONCLUSIVE"
    assert result["missing"] == ["throughput_ratio"]
    assert result["checks"]["throughput_ratio"][0] is None


def test_single_boolean_qps_input_is_inconclusive() -> None:
    result = evaluate(complete_summary(effective_qps=True))
    assert result["verdict"] == "INCONCLUSIVE"
    assert result["missing"] == ["throughput_ratio"]


def test_boolean_success_rate_is_inconclusive() -> None:
    result = evaluate(complete_summary(http_success_rate=True))
    assert result["verdict"] == "INCONCLUSIVE"
    assert result["missing"] == ["http_success_rate"]
    assert result["failed"] == []


def test_boolean_drop_rate_is_inconclusive() -> None:
    result = evaluate(complete_summary(drop_rate=True))
    assert result["verdict"] == "INCONCLUSIVE"
    assert result["missing"] == ["drop_rate"]
    assert result["failed"] == []


def test_boolean_completeness_and_accuracy_are_inconclusive() -> None:
    for field in ("trace_completeness", "token_accuracy"):
        result = evaluate(complete_summary(**{field: True}))
        assert result["verdict"] == "INCONCLUSIVE", field
        assert result["missing"] == [field], field


def test_integer_lifecycle_values_are_inconclusive() -> None:
    for field in ("process_survived", "runtime_clean"):
        result = evaluate(complete_summary(**{field: 1}))
        assert result["verdict"] == "INCONCLUSIVE", field
        assert result["missing"] == [field], field
        assert result["checks"][field][0] is None, field


def test_non_finite_latency_is_inconclusive() -> None:
    for value in (math.nan, math.inf, -math.inf):
        result = evaluate(complete_summary(latency_ms={"p99": value}))
        assert result["verdict"] == "INCONCLUSIVE", value
        assert result["missing"] == ["latency_p99_ms"], value
        assert result["failed"] == [], value


def test_non_finite_throughput_inputs_are_inconclusive() -> None:
    for field in ("input_qps", "effective_qps"):
        for value in (math.nan, math.inf):
            result = evaluate(complete_summary(**{field: value}))
            assert result["verdict"] == "INCONCLUSIVE", (field, value)
            assert result["missing"] == ["throughput_ratio"], (field, value)


def test_non_finite_success_rate_is_inconclusive() -> None:
    for value in (math.nan, math.inf):
        result = evaluate(complete_summary(http_success_rate=value))
        assert result["verdict"] == "INCONCLUSIVE", value
        assert result["missing"] == ["http_success_rate"], value


def test_non_finite_resource_measurement_is_inconclusive() -> None:
    summary = complete_summary(
        resources={"rss_mb": {"avg": 50.0, "max": math.inf, "slope_per_hour": 1.0}}
    )
    result = evaluate(summary)
    assert result["verdict"] == "INCONCLUSIVE"
    assert result["missing"] == ["rss_max_mb"]


def test_soak_non_finite_slope_is_inconclusive() -> None:
    summary = complete_summary(
        resources={
            "rss_mb": {"avg": 50.0, "max": 60.0, "slope_per_hour": math.inf},
            "file_descriptors": {"slope_per_hour": 0.0},
            "threads": {"slope_per_hour": 0.0},
            "active_connections": {"slope_per_hour": 0.0},
        }
    )
    result = evaluate(summary, scenario="soak")
    assert result["verdict"] == "INCONCLUSIVE"
    assert result["missing"] == ["rss_slope_mb_per_hour"]


def test_complete_summary_still_passes() -> None:
    result = evaluate(complete_summary())
    assert result["verdict"] == "PASS"
    assert result["missing"] == []
    assert result["failed"] == []


def test_genuine_threshold_failure_still_fails() -> None:
    result = evaluate(complete_summary(latency_ms={"p99": 1000.0}))
    assert result["verdict"] == "FAIL"
    assert result["failed"] == ["latency_p99_ms"]


def test_false_lifecycle_still_fails_rather_than_goes_missing() -> None:
    result = evaluate(complete_summary(process_survived=False))
    assert result["verdict"] == "FAIL"
    assert result["failed"] == ["process_survived"]
    assert result["missing"] == []


def test_none_measurement_still_goes_missing() -> None:
    result = evaluate(complete_summary(token_accuracy=None))
    assert result["verdict"] == "INCONCLUSIVE"
    assert result["missing"] == ["token_accuracy"]
