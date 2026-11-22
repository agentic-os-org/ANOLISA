"""A k6 record whose metric name is not a string must not abort the report.

A valid JSONL record can carry an array or object in ``metric``; the unhashable
name used to raise ``TypeError`` inside the LOAD_METRICS lookup and prevented
the Markdown report and JSON summary from being written even when neighboring
records contained usable evidence.
"""

from __future__ import annotations

import gzip
import json
import subprocess
import sys
from pathlib import Path

import pytest
from test_campaign_evidence import render_report

METRICS_CSV = (
    b"timestamp,input_qps,process_alive,cpu_pct,rss_mb,"
    b"ring_buffer_dropped,channel_dropped,completed\n"
    b"0,2,1,10,20,0,0,0\n"
    b"6,2,1,20,30,0,0,12\n"
)


def point(metric, value: int) -> bytes:
    return (
        json.dumps({"type": "Point", "metric": metric, "data": {"value": value}})
        + "\n"
    ).encode()


def clean_payload() -> bytes:
    return (
        point("benchmark_requests", 12)
        + point("benchmark_http_success", 12)
        + point("benchmark_latency", 5)
    )


def broken_payload(broken_metric) -> bytes:
    return (
        point("benchmark_requests", 12)
        + point(broken_metric, 1)
        + point("benchmark_http_success", 12)
        + point("benchmark_latency", 5)
    )


def artifact(path: Path, compressed: bool, payload: bytes) -> Path:
    if compressed:
        path = path.with_suffix(path.suffix + ".gz")
        with gzip.open(path, "wb") as handle:
            handle.write(payload)
    else:
        path.write_bytes(payload)
    return path


def write_inputs(tmp_path: Path, compressed: bool, payload: bytes) -> Path:
    tmp_path.mkdir(parents=True, exist_ok=True)
    path = artifact(tmp_path / "load.jsonl", compressed, payload)
    (tmp_path / "metrics.csv").write_bytes(METRICS_CSV)
    return path


def run_cli(tmp_path: Path, load_path: Path) -> dict:
    output, summary = tmp_path / "report.md", tmp_path / "summary.json"
    result = subprocess.run(
        [
            sys.executable,
            render_report.__file__,
            "--protocol",
            "json",
            "--qps",
            "2",
            "--duration",
            "6",
            "--metrics",
            str(tmp_path / "metrics.csv"),
            "--load-results",
            str(load_path),
            "--output",
            str(output),
            "--summary-output",
            str(summary),
        ],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 0, result.stderr
    assert output.exists()
    return json.loads(summary.read_text())


@pytest.mark.parametrize("compressed", [False, True])
@pytest.mark.parametrize("broken_metric", [["unknown"], {"unknown": 1}, None, 12])
def test_summary_matches_the_clean_input_report(
    tmp_path: Path, compressed: bool, broken_metric
) -> None:
    clean = run_cli(
        tmp_path / "clean",
        write_inputs(tmp_path / "clean", compressed, clean_payload()),
    )
    broken = run_cli(
        tmp_path / "broken",
        write_inputs(tmp_path / "broken", compressed, broken_payload(broken_metric)),
    )
    assert broken == clean
    assert broken["sent_requests"] == 12
    assert broken["successful_http_requests"] == 12
    assert broken["effective_qps"] == pytest.approx(2)
    assert broken["latency_ms"]["avg"] == pytest.approx(5)


@pytest.mark.parametrize("broken_metric", [["unknown"], {"unknown": 1}, None, 12])
def test_summarize_load_skips_non_string_metric_names(broken_metric) -> None:
    import tempfile

    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "load.jsonl"
        path.write_bytes(broken_payload(broken_metric))
        summary = render_report.summarize_load(path)
        assert summary["requests"] == 12
        assert summary["http_success"] == 12
        assert summary["timeouts"] == 0
        assert summary["latency"]["avg"] == pytest.approx(5)


def test_missing_metric_key_is_skipped(tmp_path: Path) -> None:
    path = tmp_path / "load.jsonl"
    path.write_bytes(
        point("benchmark_requests", 12)
        + b'{"type": "Point", "data": {"value": 1}}\n'
        + point("benchmark_http_success", 12)
    )
    summary = render_report.summarize_load(path)
    assert summary["requests"] == 12
    assert summary["http_success"] == 12


@pytest.mark.parametrize("compressed", [False, True])
def test_summarize_load_reads_gzip_variants(tmp_path: Path, compressed: bool) -> None:
    path = artifact(
        tmp_path / "load.jsonl",
        compressed,
        broken_payload(["unknown"]) + point("benchmark_latency", 7),
    )
    summary = render_report.summarize_load(path)
    assert summary["requests"] == 12
    assert summary["latency"]["avg"] == pytest.approx(6)


def test_report_markdown_keeps_the_latency_row(tmp_path: Path) -> None:
    load_path = write_inputs(
        tmp_path / "broken",
        False,
        broken_payload(["unknown"]),
    )
    output, summary = tmp_path / "report.md", tmp_path / "summary.json"
    result = subprocess.run(
        [
            sys.executable,
            render_report.__file__,
            "--protocol",
            "json",
            "--qps",
            "2",
            "--duration",
            "6",
            "--metrics",
            str(tmp_path / "broken" / "metrics.csv"),
            "--load-results",
            str(load_path),
            "--output",
            str(output),
            "--summary-output",
            str(summary),
        ],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 0, result.stderr
    markdown = output.read_text()
    assert "custom latency (ms)" in markdown
    assert "5.00 ms" in markdown


def test_only_broken_records_are_skipped(tmp_path: Path) -> None:
    path = tmp_path / "load.jsonl"
    path.write_bytes(
        broken_payload(["unknown"])
        + point("benchmark_requests", 12)
        + point({"nested": 1}, 1)
    )
    summary = render_report.summarize_load(path)
    assert summary["requests"] == 24
    assert summary["http_success"] == 12
