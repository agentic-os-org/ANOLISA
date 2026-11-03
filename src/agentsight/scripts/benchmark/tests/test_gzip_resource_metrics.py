from pathlib import Path
import sys
import pytest

ROOT = Path(__file__).resolve().parents[5]
BENCH = ROOT / "src/agentsight/scripts/benchmark"
sys.path.insert(0, str(BENCH / "single_run"))
sys.path.insert(0, str(BENCH / "campaign"))

import gzip
import json
import subprocess
import campaign_evidence as evidence
import render_report as report

CSV = "timestamp,input_qps,cpu_pct,rss_mb,bpf_drop_total\n1,20,10,30,0\n2,20,20,40,1\n"


@pytest.mark.parametrize(
    "reader",
    [
        report.summarize_resources,
        report.summarize_qps_resources,
        lambda path: report.summarize_drops(path, 10),
    ],
)
def test_compressed_metrics_match_plain_reporting(tmp_path, reader):
    plain, packed = tmp_path / "metrics.csv", tmp_path / "metrics.csv.gz"
    plain.write_text(CSV, encoding="utf-8")
    with gzip.open(packed, "wt", encoding="utf-8", newline="") as stream:
        stream.write(CSV)
    assert reader(packed) == reader(plain)


def test_campaign_recovery_reads_gzip_fallback(tmp_path):
    run = tmp_path / "run"
    measurement = run / "measurement"
    measurement.mkdir(parents=True)
    with gzip.open(measurement / "metrics.csv.gz", "wt", encoding="utf-8") as stream:
        stream.write(CSV)
    assert evidence.resource_samples(run / "run-result.json", "rss_mb") == [
        (1.0, 30.0),
        (2.0, 40.0),
    ]


def test_plain_campaign_artifact_takes_priority(tmp_path):
    measurement = tmp_path / "measurement"
    measurement.mkdir()
    (measurement / "metrics.csv").write_text(CSV, encoding="utf-8")
    (measurement / "metrics.csv.gz").write_bytes(b"not gzip")
    assert evidence.resource_samples(tmp_path / "run-result.json", "rss_mb") == [
        (1.0, 30.0),
        (2.0, 40.0),
    ]


def test_real_report_cli_accepts_gzip_metrics(tmp_path):
    packed = tmp_path / "metrics.csv.gz"
    with gzip.open(packed, "wt", encoding="utf-8") as stream:
        stream.write(CSV)
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
            "20",
            "--duration",
            "2",
            "--metrics",
            str(packed),
        ],
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr
    assert (
        json.loads(summary.read_text(encoding="utf-8"))["resources"]["rss_mb"]["avg"]
        == 35.0
    )
    assert packed.is_file()
