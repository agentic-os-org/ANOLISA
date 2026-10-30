"""Missing request measurements must not become observed zero throughput."""

from __future__ import annotations

import gzip
import json
import subprocess
import sys
from pathlib import Path
from typing import Any

import pytest

BENCHMARK = Path(__file__).parents[1]


def write_points(path: Path, points: list[dict[str, Any]], compressed: bool) -> Path:
    """Write only local synthetic load artifacts."""
    data = "\n".join(json.dumps(item) for item in points) + "\n"
    if compressed:
        path = path.with_suffix(path.suffix + ".gz")
        with gzip.open(path, "wt", encoding="utf-8") as stream:
            stream.write(data)
    else:
        path.write_text(data, encoding="utf-8")
    return path


def point(metric: str, value: Any) -> dict[str, Any]:
    """Use the documented JSONL point shape without running a workload."""
    return {"type": "Point", "metric": metric, "data": {"value": value}}


def run_report(
    directory: Path,
    *,
    load: Path | None = None,
    log: Path | None = None,
    protocol: str = "json",
    duration: str = "6",
) -> tuple[dict[str, Any], str]:
    """Exercise the actual report CLI with synthetic CSV and optional artifacts."""
    directory.mkdir()
    metrics = directory / "metrics.csv"
    metrics.write_text(
        "timestamp,input_qps,process_alive,cpu_pct,rss_mb,"
        "ring_buffer_dropped,channel_dropped,completed\n"
        "0,2,1,10,20,0,0,0\n6,2,1,20,30,0,0,12\n",
        encoding="utf-8",
    )
    summary, report = directory / "summary.json", directory / "report.md"
    command = [
        sys.executable,
        str(BENCHMARK / "single_run/render_report.py"),
        "--output",
        str(report),
        "--summary-output",
        str(summary),
        "--protocol",
        protocol,
        "--qps",
        "2",
        "--duration",
        duration,
        "--metrics",
        str(metrics),
    ]
    if load is not None:
        command.extend(["--load-results", str(load)])
    if log is not None:
        command.extend(["--load-log", str(log)])
    result = subprocess.run(command, capture_output=True, text=True, check=False)
    assert result.returncode == 0, result.stderr
    return json.loads(summary.read_text(encoding="utf-8")), report.read_text(encoding="utf-8")


@pytest.mark.parametrize("compressed", [False, True])
@pytest.mark.parametrize(
    "shape",
    [
        "missing",
        "empty",
        "malformed",
        "latency-only",
        "other-counter",
        "null",
        "text",
        "boolean",
        "negative",
        "fractional",
    ],
)
def test_unmeasured_request_count_does_not_become_zero_qps(
    tmp_path: Path, shape: str, compressed: bool
) -> None:
    artifact = tmp_path / "load.jsonl"
    points = {
        "empty": [],
        "latency-only": [point("benchmark_latency", 5)],
        "other-counter": [point("unrelated_counter", 12)],
        "null": [point("benchmark_requests", None)],
        "text": [point("benchmark_requests", "unknown")],
        "boolean": [point("benchmark_requests", True)],
        "negative": [point("benchmark_requests", -1)],
        "fractional": [point("benchmark_requests", 0.5)],
    }
    if shape == "missing":
        if compressed:
            artifact = artifact.with_suffix(".jsonl.gz")
    elif shape == "malformed":
        artifact = write_points(artifact, [], compressed)
        if compressed:
            with gzip.open(artifact, "wt", encoding="utf-8") as stream:
                stream.write("not-json\n")
        else:
            artifact.write_text("not-json\n", encoding="utf-8")
    else:
        artifact = write_points(artifact, points[shape], compressed)
    summary, markdown = run_report(tmp_path / "output", load=artifact)
    assert summary["effective_qps"] is None
    assert summary["resources"]["rss_mb"]["avg"] == 25
    if shape == "latency-only":
        assert summary["latency_ms"]["avg"] == 5
        assert "0.00 req/s" not in markdown


def test_omitted_load_path_is_unknown(tmp_path: Path) -> None:
    summary, markdown = run_report(tmp_path / "output")
    assert summary["effective_qps"] is None
    assert "No load summary" in markdown


@pytest.mark.parametrize("compressed", [False, True])
@pytest.mark.parametrize("requests, expected", [(0, 0.0), (12, 2.0)])
def test_observed_zero_and_nonzero_request_counts_remain_measurements(
    tmp_path: Path, compressed: bool, requests: int, expected: float
) -> None:
    load = write_points(
        tmp_path / "load.jsonl",
        [point("benchmark_requests", requests), point("benchmark_http_success", requests)],
        compressed,
    )
    summary, markdown = run_report(tmp_path / "output", load=load)
    assert summary["effective_qps"] == expected
    assert f"{expected:.2f} req/s" in markdown


@pytest.mark.parametrize("compressed", [False, True])
def test_unusable_record_does_not_discard_other_valid_request_points(
    tmp_path: Path, compressed: bool
) -> None:
    load = write_points(
        tmp_path / "load.jsonl",
        [
            point("benchmark_requests", 6),
            point("benchmark_requests", None),
            point("benchmark_requests", 6),
        ],
        compressed,
    )
    summary, _ = run_report(tmp_path / "output", load=load)
    assert summary["effective_qps"] == 2.0


@pytest.mark.parametrize("duration", ["0", "-1", "nan", "unknown"])
def test_unusable_duration_does_not_derive_a_counter_rate(tmp_path: Path, duration: str) -> None:
    load = write_points(tmp_path / "load.jsonl", [point("benchmark_requests", 12)], False)
    summary, _ = run_report(tmp_path / "output", load=load, duration=duration)
    assert summary["effective_qps"] is None


@pytest.mark.parametrize("rate", [0.0, 2.0])
@pytest.mark.parametrize("duration", ["0", "6"])
def test_recorded_h2load_throughput_remains_usable_without_request_row(
    tmp_path: Path, rate: float, duration: str
) -> None:
    log = tmp_path / "h2load.txt"
    log.write_text(f"finished in 6.00s, {rate:.2f} req/s, 10KB/s\n", encoding="utf-8")
    summary, _ = run_report(tmp_path / "output", log=log, protocol="h2", duration=duration)
    assert summary["effective_qps"] == rate


def test_h2load_request_count_without_rate_uses_valid_duration(tmp_path: Path) -> None:
    log = tmp_path / "h2load.txt"
    log.write_text(
        "requests: 12 total, 12 started, 12 done, 12 succeeded, 0 failed, 0 errored, 0 timeout\n",
        encoding="utf-8",
    )
    summary, _ = run_report(tmp_path / "output", log=log, protocol="h2")
    assert summary["effective_qps"] == 2.0


@pytest.mark.parametrize("shape", ["missing", "empty", "unrecognized"])
def test_h2load_without_a_request_or_throughput_measurement_is_unknown(
    tmp_path: Path, shape: str
) -> None:
    log = tmp_path / "h2load.txt"
    if shape != "missing":
        log.write_text("" if shape == "empty" else "unrecognized summary\n", encoding="utf-8")
    summary, _ = run_report(tmp_path / "output", log=log, protocol="h2")
    assert summary["effective_qps"] is None
