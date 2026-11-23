"""Signed RSS trends must not reverse the reported optimization direction."""

from pathlib import Path

import pytest
from test_campaign_matrix_evidence import aggregate_report


def slope_comparison(baseline: float | None, optimized: float | None) -> dict:
    items = [
        (
            Path(f"synthetic/{version}/run-result.json"),
            {
                "scenario": "soak",
                "version": version,
                "summary": {"resources": {"rss_mb": {"slope_per_hour": slope}}},
            },
        )
        for version, slope in (("baseline", baseline), ("optimized", optimized))
    ]
    return aggregate_report.comparison_summary([], {}, items)


@pytest.mark.parametrize(
    ("baseline", "optimized", "expected"),
    [
        (-1.0, 1.0, "长期内存增长提高（回退）"),
        (-1.0, -2.0, "长期内存增长降低"),
        (-2.0, -1.0, "长期内存增长提高（回退）"),
        (1.0, 2.0, "长期内存增长提高（回退）"),
        (2.0, 1.0, "长期内存增长降低"),
        (-1.0, -1.0, "长期内存增长基本不变"),
        (0.0, 1.0, "证据不足"),
        (None, 1.0, "证据不足"),
    ],
)
def test_soak_direction_uses_signed_measurements(
    baseline: float | None, optimized: float | None, expected: str
) -> None:
    comparison = slope_comparison(baseline, optimized)
    row = aggregate_report.headline_lines(comparison)[-1]
    assert f"| {expected} |" in row
    metric = comparison["metrics"]["soak_rss_slope_mb_per_hour"]
    expected_percent = (
        (optimized - baseline) / baseline * 100
        if baseline not in (None, 0) and optimized is not None
        else None
    )
    assert metric["delta_pct"] == expected_percent


@pytest.mark.parametrize("options", [{"aa_calibration": True}, {"claims_allowed": False}])
def test_signed_slopes_preserve_claim_restrictions(options: dict) -> None:
    row = aggregate_report.headline_lines(slope_comparison(-1.0, 1.0), **options)[-1]
    assert "长期内存增长提高" not in row
    assert "长期内存增长降低" not in row
    assert "A/A 测量波动" in row or "证据不足" in row
