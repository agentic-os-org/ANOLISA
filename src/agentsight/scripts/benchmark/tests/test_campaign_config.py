"""Contract tests for the frozen campaign configuration validator.

`campaign_config.validate_campaign` is the gate every formal benchmark
campaign passes before a single request is fired: an unsafe or internally
inconsistent config must be rejected here, not discovered mid-campaign.
"""

from __future__ import annotations

import copy
import json
import math
import sys
from pathlib import Path

import pytest

BENCHMARK_DIR = Path(__file__).parents[1]
sys.path.insert(0, str(BENCHMARK_DIR / "campaign"))

from campaign_config import (  # noqa: E402
    DEFAULT_SAFETY,
    REQUIRED_THRESHOLDS,
    finite_number,
    safety_settings,
    validate_campaign,
)

EXAMPLE = json.loads((BENCHMARK_DIR / "campaign/campaign.example.json").read_text())


def valid() -> dict:
    """A deep copy of the canonical example, safe to mutate per test."""
    return copy.deepcopy(EXAMPLE)


def test_accepts_the_canonical_example_campaign() -> None:
    validate_campaign(valid())


def test_finite_number_rejects_everything_but_real_finite_numbers() -> None:
    assert finite_number(1)
    assert finite_number(1.5)
    assert finite_number(0)
    # bool is an int subclass but is not a threshold value
    assert not finite_number(True)
    assert not finite_number("1")
    assert not finite_number(None)
    assert not finite_number(math.nan)
    assert not finite_number(math.inf)
    # an int too large for float conversion must not crash the validator
    assert not finite_number(10**400)


def test_safety_settings_fill_defaults_from_old_campaigns() -> None:
    legacy = valid()
    legacy.pop("safety")
    assert safety_settings(legacy) == DEFAULT_SAFETY

    partial = valid()
    partial["safety"] = {"max_k6_vus": 128}
    filled = safety_settings(partial)
    assert filled["max_k6_vus"] == 128
    assert filled["max_results_gb"] == DEFAULT_SAFETY["max_results_gb"]


def test_safety_settings_reject_non_object_safety() -> None:
    campaign = valid()
    campaign["safety"] = [1, 2]
    with pytest.raises(TypeError, match="safety must be an object"):
        safety_settings(campaign)


def test_schema_version_and_comparison_mode_are_frozen() -> None:
    for schema_version in (2, "1", None):
        campaign = valid()
        campaign["schema_version"] = schema_version
        with pytest.raises(ValueError, match="schema_version must be 1"):
            validate_campaign(campaign)

    campaign = valid()
    campaign["comparison_mode"] = "abc"
    with pytest.raises(ValueError, match="comparison_mode"):
        validate_campaign(campaign)

    # the default is the A/B comparison
    campaign = valid()
    campaign.pop("comparison_mode")
    validate_campaign(campaign)
    campaign["comparison_mode"] = "aa_calibration"
    validate_campaign(campaign)


def test_versions_must_be_exactly_baseline_and_optimized_with_real_paths() -> None:
    campaign = valid()
    del campaign["versions"]["optimized"]
    with pytest.raises(ValueError, match="versions must contain exactly"):
        validate_campaign(campaign)

    campaign = valid()
    campaign["versions"]["baseline"]["commit"] = "   "
    with pytest.raises(ValueError, match="versions.baseline.commit"):
        validate_campaign(campaign)

    # optional result files must be real paths when set
    campaign = valid()
    campaign["versions"]["baseline"]["metrics_file"] = ""
    with pytest.raises(ValueError, match="metrics_file"):
        validate_campaign(campaign)


def test_missing_thresholds_are_named_sorted_in_the_error() -> None:
    campaign = valid()
    del campaign["thresholds"]["max_p99_ms"]
    del campaign["thresholds"]["min_token_accuracy"]
    with pytest.raises(
        ValueError,
        match=r"missing frozen thresholds: max_p99_ms, min_token_accuracy",
    ):
        validate_campaign(campaign)


@pytest.mark.parametrize(
    "name,value",
    [
        ("max_p99_ms", -1),
        ("max_p99_ms", math.nan),
        ("max_p99_ms", "1000"),
        ("max_p99_ms", True),
    ],
)
def test_thresholds_must_be_finite_non_negative_numbers(name: str, value: object) -> None:
    campaign = valid()
    campaign["thresholds"][name] = value
    with pytest.raises(ValueError, match=f"thresholds.{name}"):
        validate_campaign(campaign)


def test_ratio_thresholds_are_capped_at_one_but_absolute_ones_are_not() -> None:
    campaign = valid()
    campaign["thresholds"]["min_throughput_ratio"] = 1.5
    with pytest.raises(ValueError, match="between 0 and 1"):
        validate_campaign(campaign)

    # max_p99_ms is an absolute bound, not a ratio: 1.5 ms is a valid (if tiny) cap
    campaign = valid()
    campaign["thresholds"]["max_p99_ms"] = 1.5
    validate_campaign(campaign)


def test_capacity_window_must_be_well_formed() -> None:
    campaign = valid()
    campaign["capacity"]["qps_start"] = 0
    with pytest.raises(ValueError, match="qps_start and qps_resolution must be positive"):
        validate_campaign(campaign)

    campaign = valid()
    campaign["capacity"]["qps_safety_max"] = campaign["capacity"]["qps_start"] - 1
    with pytest.raises(ValueError, match="qps_safety_max must be at least qps_start"):
        validate_campaign(campaign)


def test_capacity_search_start_ratio_is_frozen_at_zero_point_eight() -> None:
    campaign = valid()
    campaign["capacity"]["search_start_ratio"] = 0.75
    with pytest.raises(ValueError, match="search_start_ratio must be 0.8"):
        validate_campaign(campaign)


@pytest.mark.parametrize(
    "qps",
    [
        [10, 20, 30, 40],
        [10, 10, 20, 30, 40],
        [10, 20, 30, 40, 0],
        [10, 20, 30, 40, 50.5],
        [True, 20, 30, 40, 50],
    ],
)
def test_matrix_qps_is_empty_or_exactly_five_unique_positive_integers(qps: list) -> None:
    campaign = valid()
    campaign["matrix"]["qps"] = qps
    with pytest.raises(ValueError, match="matrix.qps"):
        validate_campaign(campaign)


def test_bools_are_not_positive_integers_anywhere() -> None:
    campaign = valid()
    campaign["smoke"]["qps"] = True
    with pytest.raises(ValueError, match="smoke.qps must be a positive integer"):
        validate_campaign(campaign)


def test_warmup_seconds_may_be_zero_but_not_negative() -> None:
    campaign = valid()
    campaign["soak"]["warmup_seconds"] = 0
    validate_campaign(campaign)

    campaign = valid()
    campaign["soak"]["warmup_seconds"] = -1
    with pytest.raises(ValueError, match="warmup_seconds"):
        validate_campaign(campaign)


def test_safety_rss_cap_must_cover_the_threshold_rss_budget() -> None:
    campaign = valid()
    campaign["safety"]["max_agentsight_rss_mb"] = (
        campaign["thresholds"]["max_rss_mb"] - 1
    )
    with pytest.raises(ValueError, match="max_agentsight_rss_mb must be at least"):
        validate_campaign(campaign)


def test_recovery_window_cannot_outlast_the_recovery_phase() -> None:
    campaign = valid()
    campaign["recovery"]["recovery_window_seconds"] = (
        campaign["recovery"]["recover_seconds"] + 1
    )
    with pytest.raises(ValueError, match="recovery window cannot exceed"):
        validate_campaign(campaign)


def test_recovery_tolerance_ratio_is_half_open() -> None:
    campaign = valid()
    campaign["recovery"]["tolerance_ratio"] = 1
    with pytest.raises(ValueError, match=r"tolerance_ratio must be in \[0, 1\)"):
        validate_campaign(campaign)

    # 0 is inside the interval
    campaign = valid()
    campaign["recovery"]["tolerance_ratio"] = 0
    validate_campaign(campaign)


def test_load_protocol_is_frozen_to_sse_or_json() -> None:
    campaign = valid()
    campaign["load"]["protocol"] = "grpc"
    with pytest.raises(ValueError, match="load.protocol"):
        validate_campaign(campaign)


def test_every_required_threshold_is_actually_enforced() -> None:
    # a guard against the frozen set and the validator drifting apart
    assert REQUIRED_THRESHOLDS == set(EXAMPLE["thresholds"])
