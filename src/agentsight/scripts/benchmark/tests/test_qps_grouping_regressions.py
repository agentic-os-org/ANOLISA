"""Verify QPS resource grouping keeps distinct fractional rates separate."""

from __future__ import annotations

import gzip
import json
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[5]
BENCH = ROOT / "src/agentsight/scripts/benchmark"
sys.path.insert(0, str(BENCH / "single_run"))

import render_report as report

# Fractional rates that the %g display format used to collapse into one row.
CSV = (
    "timestamp,input_qps,cpu_pct,rss_mb\n"
    "1,0.00001234562,40,80\n"
    "2,0.00001234561,20,40\n"
    "3,1.234561e-05,30,60\n"
    "4,10.00002,80,160\n"
    "5,10.00001,50,100\n"
)


def write_metrics(directory: Path, text: str) -> Path:
    path = directory / "metrics.csv"
    path.write_text(text, encoding="utf-8")
    return path


def test_distinct_fractional_rates_get_separate_rows(tmp_path) -> None:
    grouped = report.summarize_qps_resources(write_metrics(tmp_path, CSV))
    assert set(grouped) == {"1.234562e-05", "1.234561e-05", "10.00002", "10.00001"}
    assert grouped["1.234562e-05"]["samples"] == 1
    assert grouped["1.234561e-05"]["samples"] == 2
    assert grouped["1.234561e-05"]["cpu_pct"]["avg"] == pytest.approx(25.0)
    assert grouped["10.00002"]["cpu_pct"]["avg"] == pytest.approx(80.0)
    assert grouped["10.00001"]["cpu_pct"]["avg"] == pytest.approx(50.0)


def test_equivalent_spellings_share_one_group(tmp_path) -> None:
    text = "timestamp,input_qps,cpu_pct,rss_mb\n1,0.00001234561,20,40\n2,1.234561e-05,30,60\n"
    grouped = report.summarize_qps_resources(write_metrics(tmp_path, text))
    assert list(grouped) == ["1.234561e-05"]
    assert grouped["1.234561e-05"]["samples"] == 2


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        ("2", "2"),
        ("10", "10"),
        ("10.0", "10"),
        ("10.5", "10.5"),
        ("0.00001234562", "1.234562e-05"),
        ("10.00002", "10.00002"),
        ("not-a-number", "unknown"),
        ("", "unknown"),
    ],
)
def test_qps_key_display_is_stable(raw: str, expected: str) -> None:
    assert report.qps_key(raw) == expected


def test_qps_rows_stay_numerically_sorted(tmp_path) -> None:
    grouped = report.summarize_qps_resources(write_metrics(tmp_path, CSV))
    ordered = sorted(grouped, key=report.qps_sort_key)
    values = [float(value) for value in ordered]
    assert values == sorted(values)
    assert ordered[0] == "1.234561e-05"
    assert ordered[-1] == "10.00002"


@pytest.mark.parametrize("compress", [False, True])
def test_report_cli_groups_fractional_qps(tmp_path, compress: bool) -> None:
    metrics = write_metrics(tmp_path, CSV)
    if compress:
        packed = tmp_path / "metrics.csv.gz"
        with gzip.open(packed, "wt", encoding="utf-8", newline="") as stream:
            stream.write(CSV)
        metrics = packed
    summary = tmp_path / "summary.json"
    result = subprocess.run(
        [
            sys.executable,
            str(BENCH / "single_run/render_report.py"),
            "--output",
            str(tmp_path / "report.md"),
            "--summary-output",
            str(summary),
            "--protocol",
            "sse",
            "--qps",
            "10",
            "--duration",
            "2",
            "--metrics",
            str(metrics),
        ],
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr
    report_text = (tmp_path / "report.md").read_text(encoding="utf-8")
    section = report_text.split("## QPS to CPU and memory", 1)[1].split("\n## ", 1)[0]
    table = [
        line
        for line in section.splitlines()
        if line.startswith("| ") and "Input QPS" not in line and set(line) != {"|", " ", "-", ":"}
    ]
    keys = [row.split("|")[1].strip() for row in table]
    assert keys == ["1.234561e-05", "1.234562e-05", "10.00001", "10.00002"]
    samples = [row.split("|")[2].strip() for row in table]
    assert samples == ["2", "1", "1", "1"]
    assert json.loads(summary.read_text(encoding="utf-8"))["protocol"] == "sse"
