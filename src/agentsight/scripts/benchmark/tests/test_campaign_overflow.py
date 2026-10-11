"""Overflow tolerance for campaign aggregation over metrics artifacts.

Campaign artifacts are read with plain ``json.loads`` (via ``read_json``),
which happily produces arbitrary-precision integers. A corrupted counter
such as ``10**1000`` makes ``float()`` and ``math.isfinite()`` raise
``OverflowError``, which crashed report aggregation (``nested``), the quick
overhead comparison (``quick_value``) and the measurement gates
(``numeric_measurement``), while the single-run ``benchmark_stats.numeric``
and ``campaign_config.finite_number`` already tolerate the same outliers.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

BENCHMARK_DIR = Path(__file__).parents[1]
sys.path.insert(0, str(BENCHMARK_DIR / "campaign"))

import aggregate_report
import campaign_evaluation
import run_campaign

HUGE = json.loads("1" + "0" * 1000)


def test_nested_ignores_overflowing_integers() -> None:
    assert aggregate_report.nested({"rss_mb": {"max": HUGE}}, "rss_mb", "max") is None


def test_nested_ignores_overflowing_json_outliers() -> None:
    # json.dumps/json.loads round-trips arbitrary-precision integers,
    # exactly like a metrics artifact written by a corrupted counter.
    artifact = json.dumps({'resources': {'rss_mb': {'max': HUGE}}})
    summary = json.loads(artifact)
    assert aggregate_report.nested(summary, 'resources', 'rss_mb', 'max') is None


def test_nested_keeps_ordinary_values_and_booleans() -> None:
    assert aggregate_report.nested({"rss_mb": {"max": 5.5}}, "rss_mb", "max") == 5.5
    assert aggregate_report.nested({"gate": True}, "gate") is True
    assert aggregate_report.nested({"name": "agent"}, "name") is None


def test_numeric_measurement_ignores_overflowing_integers() -> None:
    assert campaign_evaluation.numeric_measurement(HUGE) is None


def test_numeric_measurement_keeps_usable_values() -> None:
    assert campaign_evaluation.numeric_measurement(7) == 7.0
    assert campaign_evaluation.numeric_measurement(0.5) == 0.5
    assert campaign_evaluation.numeric_measurement(True) is None


def test_quick_value_ignores_overflowing_integers() -> None:
    assert run_campaign.quick_value({"summary": {"x": HUGE}}, ("summary", "x")) is None


def test_quick_value_keeps_ordinary_values() -> None:
    assert run_campaign.quick_value({"summary": {"x": 3.5}}, ("summary", "x")) == 3.5
    assert run_campaign.quick_value({"summary": {}}, ("summary", "x")) is None
