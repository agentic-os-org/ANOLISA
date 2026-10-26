"""Unreadable procfs CPU counters must not become idle CPU evidence."""

import os
from pathlib import Path

import pytest
from test_benchmark import collect_metrics


@pytest.mark.parametrize("text", ["", "123 (truncated)", "123 (worker) R " + "bad " * 15])
def test_malformed_stat_has_no_cpu_measurement(monkeypatch: pytest.MonkeyPatch, text: str) -> None:
    monkeypatch.setattr(Path, "read_text", lambda *args, **kwargs: text)
    assert collect_metrics.read_cpu_ticks(123) is None


def test_unreadable_stat_has_no_cpu_measurement(monkeypatch: pytest.MonkeyPatch) -> None:
    def denied(*args: object, **kwargs: object) -> str:
        raise PermissionError("fixture procfs access denied")

    monkeypatch.setattr(Path, "read_text", denied)
    assert collect_metrics.read_cpu_ticks(123) is None


@pytest.mark.parametrize("ticks", [None, 50])
def test_missing_or_rewound_counter_invalidates_interval(
    monkeypatch: pytest.MonkeyPatch, ticks: int | None
) -> None:
    monkeypatch.setattr(collect_metrics, "read_status", lambda pid: {"VmRSS": 1024, "Threads": 1})
    monkeypatch.setattr(collect_metrics, "read_cpu_ticks", lambda pid: ticks)
    monkeypatch.setattr(collect_metrics.time, "monotonic", lambda: 11)
    row, baseline, _ = collect_metrics.sample(123, 100, 10, 1, None)
    assert row["cpu_pct"] == ""
    assert row["process_alive"] == 1
    assert row["rss_mb"] == "1.000"
    assert baseline == ticks


def test_recovery_reestablishes_baseline_before_measuring(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(collect_metrics, "read_status", lambda pid: {"VmRSS": 1024})
    counter = iter([None, 120, 130])
    clock = iter([11, 12, 13])
    monkeypatch.setattr(collect_metrics, "read_cpu_ticks", lambda pid: next(counter))
    monkeypatch.setattr(collect_metrics.time, "monotonic", lambda: next(clock))
    first, ticks, now = collect_metrics.sample(123, 100, 10, 1, None)
    second, ticks, now = collect_metrics.sample(123, ticks, now, 1, None)
    third, _, _ = collect_metrics.sample(123, ticks, now, 1, None)
    assert first["cpu_pct"] == second["cpu_pct"] == ""
    assert float(third["cpu_pct"]) == pytest.approx(1000 / os.sysconf("SC_CLK_TCK"))


def test_real_process_cpu_counter_control() -> None:
    assert collect_metrics.read_cpu_ticks(os.getpid()) is not None


def test_real_absent_process_has_no_cpu_counter() -> None:
    assert collect_metrics.read_cpu_ticks(30000000) is None


def test_missing_cpu_read_cannot_prove_resource_improvement(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from test_resource_report import resource_report

    monkeypatch.setattr(collect_metrics, "read_status", lambda pid: {"VmRSS": 1024})
    monkeypatch.setattr(collect_metrics, "read_cpu_ticks", lambda pid: None)
    monkeypatch.setattr(collect_metrics.time, "monotonic", lambda: 11)
    sampled, _, _ = collect_metrics.sample(123, 100, 10, 1, None)
    rows = [{key: str(value) for key, value in sampled.items()}]
    assert not resource_report.resource_stats(rows, 10, 11)["cpu_samples_valid"]
