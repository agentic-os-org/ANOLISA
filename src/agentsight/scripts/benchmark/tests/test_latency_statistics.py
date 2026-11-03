"""Latency summaries keep exact semantics while sorting one snapshot once."""

import builtins
import json
from pathlib import Path
from statistics import fmean

import pytest
from test_benchmark import render_report
from test_benchmark_stats import benchmark_stats


@pytest.mark.parametrize(
    "values", [[], [7], [9, 1, 2, 8], [0, 0, 3, 3, 100], list(range(100, 0, -1))]
)
def test_stats_matches_independent_nearest_rank_reference(values: list[float]) -> None:
    import math

    ordered = sorted(values)
    expected = {key: None for key in ("first", "last", "min", "avg", "p50", "p95", "p99", "max")}
    if values:
        expected.update(
            first=values[0], last=values[-1], min=min(values), max=max(values), avg=fmean(values)
        )
        expected.update(
            {
                key: ordered[math.ceil(len(values) * fraction) - 1]
                for key, fraction in (("p50", 0.5), ("p95", 0.95), ("p99", 0.99))
            }
        )
    before = list(values)
    assert benchmark_stats.stats(values) == expected
    assert values == before


def test_stats_sorts_one_snapshot_once(monkeypatch: pytest.MonkeyPatch) -> None:
    sorts = []
    original = builtins.sorted

    def counted(values: list[float]) -> list[float]:
        sorts.append(len(values))
        return original(values)

    monkeypatch.setattr(benchmark_stats, "sorted", counted, raising=False)
    assert benchmark_stats.stats([9, 1, 5])["p50"] == 5
    assert sorts == [3]


@pytest.mark.parametrize("custom_first", [False, True])
@pytest.mark.parametrize("compressed", [False, True])
def test_report_retains_preferred_latency_and_fallback(
    tmp_path: Path, custom_first: bool, compressed: bool
) -> None:
    import gzip

    points = [("http_req_duration", 900), ("http_req_duration", 800)]
    custom = [("benchmark_latency", 20), ("benchmark_latency", 10)]
    points = custom + points if custom_first else points[:1] + custom + points[1:]
    text = "".join(
        json.dumps({"metric": name, "data": {"value": value}}) + "\n" for name, value in points
    )
    path = tmp_path / ("k6.jsonl.gz" if compressed else "k6.jsonl")
    if compressed:
        with gzip.open(path, "wt") as handle:
            handle.write(text)
    else:
        path.write_text(text)
    measured = render_report.summarize_load(path)
    assert measured["latency_metric"] == "benchmark_latency"
    assert measured["latency"] == benchmark_stats.stats([20, 10])
    report = render_report.build_summary(
        protocol="sse",
        qps="10",
        duration="2",
        load=measured,
        resources=render_report.summarize_resources(None),
        drops=render_report.summarize_drops(None, 0),
        validation=None,
    )
    assert report["latency_ms"]["p99"] == 20


def test_report_falls_back_when_all_custom_values_are_invalid(tmp_path: Path) -> None:
    path = tmp_path / "k6.jsonl"
    path.write_text(
        "".join(
            json.dumps({"metric": name, "data": {"value": value}}) + "\n"
            for name, value in [
                ("benchmark_latency", None),
                ("http_req_duration", 8),
                ("benchmark_latency", "bad"),
                ("http_req_duration", 4),
            ]
        )
    )
    result = render_report.summarize_load(path)
    assert result["latency_metric"] == "http_req_duration"
    assert result["latency"] == benchmark_stats.stats([8, 4])
