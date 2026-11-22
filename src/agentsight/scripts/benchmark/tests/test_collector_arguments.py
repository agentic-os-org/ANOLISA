"""Reject invalid collector timing before opening the metrics output."""

from __future__ import annotations

import csv
import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parents[1] / "single_run"))

import collect_metrics


@pytest.fixture
def collector_output(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    output = tmp_path / "metrics.csv"
    output.write_text("previous measurements\n", encoding="utf-8")
    original_exists = Path.exists
    monkeypatch.setattr(
        Path,
        "exists",
        lambda path: (
            True if str(path) == str(Path("/proc/123")) else original_exists(path)
        ),
    )
    monkeypatch.setattr(collect_metrics.signal, "signal", lambda *args: None)
    monkeypatch.setattr(collect_metrics, "STOP", False)
    monkeypatch.setattr(collect_metrics, "read_cpu_ticks", lambda pid: 0)
    monkeypatch.setattr(
        collect_metrics,
        "read_process_identity",
        lambda pid: collect_metrics.ProcessIdentity(pid, 1),
    )
    monkeypatch.setattr(
        collect_metrics,
        "sample",
        lambda *args: ({"timestamp": 1, "process_alive": 1}, 0, 0),
    )

    def stop_after_sample(interval: float) -> None:
        collect_metrics.STOP = True

    monkeypatch.setattr(collect_metrics.time, "sleep", stop_after_sample)
    return output


@pytest.mark.parametrize(
    ("option", "value"),
    [
        ("interval", "nan"),
        ("interval", "inf"),
        ("interval", "-inf"),
        ("interval", "0"),
        ("interval", "-1"),
        ("duration", "nan"),
        ("duration", "inf"),
        ("duration", "-inf"),
        ("duration", "-1"),
    ],
)
def test_invalid_timing_preserves_output(
    collector_output: Path, monkeypatch: pytest.MonkeyPatch, option: str, value: str
) -> None:
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "collect_metrics.py",
            "--pid=123",
            f"--output={collector_output}",
            f"--{option}={value}",
        ],
    )
    with pytest.raises(SystemExit, match=option):
        collect_metrics.main()
    assert collector_output.read_text(encoding="utf-8") == "previous measurements\n"


@pytest.mark.parametrize("duration", ["0", "1"])
def test_valid_timing_collects_samples(
    collector_output: Path, monkeypatch: pytest.MonkeyPatch, duration: str
) -> None:
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "collect_metrics.py",
            "--pid=123",
            f"--output={collector_output}",
            "--interval=0.25",
            f"--duration={duration}",
        ],
    )
    assert collect_metrics.main() == 0
    assert len(collector_output.read_text(encoding="utf-8").splitlines()) == 3
    with collector_output.open() as handle:
        first = next(csv.DictReader(handle))
    assert first["cpu_pct"] == first["cpu_interval_seconds"] == ""


def test_shutdown_sample_keeps_drop_counters_published_after_the_last_tick(
    collector_output: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    dropped = 0

    def sample(*args: object) -> tuple[dict[str, int], int, float]:
        return {"timestamp": 1, "ring_buffer_dropped": dropped}, 0, 0.0

    def stop_after_publish(_: float) -> None:
        nonlocal dropped
        dropped = 7
        collect_metrics.stop(0, None)

    monkeypatch.setattr(collect_metrics, "sample", sample)
    monkeypatch.setattr(collect_metrics.time, "sleep", stop_after_publish)
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "collect_metrics.py",
            "--pid=123",
            f"--output={collector_output}",
            "--duration=0",
        ],
    )
    assert collect_metrics.main() == 0
    with collector_output.open() as handle:
        rows = list(csv.DictReader(handle))
    assert [row["ring_buffer_dropped"] for row in rows] == ["0", "7"]


def test_start_barrier_timeout_does_not_write_baseline_or_open_metrics(
    collector_output: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    def sample(*args: object) -> tuple[dict[str, int], int, float]:
        collect_metrics.STOP = True
        return (
            {
                "monotonic_timestamp": 123,
                "counter_timestamp": 124,
                "ring_buffer_dropped": 7,
                "channel_dropped": 0,
            },
            0,
            0.0,
        )

    monkeypatch.setattr(collect_metrics, "sample", sample)
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "collect_metrics.py",
            "--pid=123",
            f"--output={collector_output}",
            f"--start-file={collector_output.parent / 'start.json'}",
        ],
    )

    assert collect_metrics.main() == 0
    assert not (collector_output.parent / "metrics-baseline.json").exists()
    assert collector_output.read_text(encoding="utf-8") == "previous measurements\n"


def test_baseline_is_written_after_sampling_and_start_barrier(
    collector_output: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    start_file = collector_output.parent / "start.json"
    sample_calls = 0
    open_calls: list[bool] = []
    baseline_writes = 0
    real_open = open
    real_write_json = collect_metrics.write_json

    def sample(*args: object) -> tuple[dict[str, int], int, float]:
        nonlocal sample_calls
        sample_calls += 1
        if sample_calls == 2:
            collect_metrics.STOP = True
        return (
            {
                "monotonic_timestamp": 123 + sample_calls,
                "ring_buffer_dropped": 7,
                "channel_dropped": 0,
            },
            0,
            0.0,
        )

    def publish_start(_: float) -> None:
        start_file.write_text(json.dumps({"start": 0}))

    def tracked_open(file: object, *args: object, **kwargs: object) -> object:
        if str(file) == str(collector_output):
            open_calls.append(start_file.exists())
        return real_open(file, *args, **kwargs)

    def tracked_write_json(path: Path, value: dict[str, object]) -> None:
        nonlocal baseline_writes
        assert sample_calls == 3
        baseline_writes += 1
        real_write_json(path, value)

    monkeypatch.setattr(collect_metrics, "sample", sample)
    monkeypatch.setattr(collect_metrics.time, "sleep", publish_start)
    monkeypatch.setattr(collect_metrics, "open", tracked_open, raising=False)
    monkeypatch.setattr(collect_metrics, "write_json", tracked_write_json)
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "collect_metrics.py",
            "--pid=123",
            f"--output={collector_output}",
            f"--start-file={start_file}",
        ],
    )

    assert collect_metrics.main() == 0
    assert open_calls == [True]
    assert baseline_writes == 1
    assert (collector_output.parent / "metrics-baseline.json").exists()
    with collector_output.open() as handle:
        assert len(handle.read().splitlines()) == 3


def test_sample_records_counter_timestamp_after_metrics_read(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    timestamps = iter([100.0, 101.0])
    monkeypatch.setattr(collect_metrics.time, "monotonic", lambda: next(timestamps))
    monkeypatch.setattr(collect_metrics, "read_status", lambda pid: {})
    monkeypatch.setattr(collect_metrics, "read_cpu_ticks", lambda pid: None)
    monkeypatch.setattr(
        collect_metrics,
        "read_internal_metrics",
        lambda path: {"ring_buffer_dropped": "1"},
    )

    row, _, _ = collect_metrics.sample(123, None, 0.0, 0.0, None)

    assert row["monotonic_timestamp"] == 100.0
    assert row["counter_timestamp"] == 101.0
