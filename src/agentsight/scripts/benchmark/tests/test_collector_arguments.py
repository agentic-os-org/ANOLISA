"""Reject invalid collector timing before opening the metrics output."""

from __future__ import annotations

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
        lambda path: True if str(path) == str(Path("/proc/123")) else original_exists(path),
    )
    monkeypatch.setattr(collect_metrics.signal, "signal", lambda *args: None)
    monkeypatch.setattr(collect_metrics, "STOP", False)
    monkeypatch.setattr(collect_metrics, "read_cpu_ticks", lambda pid: 0)
    monkeypatch.setattr(
        collect_metrics, "sample", lambda *args: ({"timestamp": 1, "process_alive": 1}, 0, 0)
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
        ["collect_metrics.py", "--pid=123", f"--output={collector_output}", f"--{option}={value}"],
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
    assert len(collector_output.read_text(encoding="utf-8").splitlines()) == 2
