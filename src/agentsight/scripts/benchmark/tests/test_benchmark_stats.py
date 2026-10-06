from __future__ import annotations

import json
import sys
from pathlib import Path

BENCHMARK_DIR = Path(__file__).parents[1]
sys.path.insert(0, str(BENCHMARK_DIR / "single_run"))

import benchmark_stats


def test_numeric_ignores_overflowing_integers() -> None:
    assert benchmark_stats.numeric(10**1000) is None


def test_numeric_ignores_overflowing_json_outliers() -> None:
    outlier = json.loads("1" + "0" * 1000)
    assert benchmark_stats.numeric(outlier) is None


def test_numeric_keeps_ordinary_values() -> None:
    assert benchmark_stats.numeric("3.5") == 3.5
    assert benchmark_stats.numeric(7) == 7.0
    assert benchmark_stats.numeric(-2.25) == -2.25


def test_numeric_still_rejects_booleans_and_malformed_values() -> None:
    assert benchmark_stats.numeric(True) is None
    assert benchmark_stats.numeric(False) is None
    assert benchmark_stats.numeric("not-a-number") is None
    assert benchmark_stats.numeric(None) is None
    assert benchmark_stats.numeric(float("nan")) is None
    assert benchmark_stats.numeric("1e999") is None
