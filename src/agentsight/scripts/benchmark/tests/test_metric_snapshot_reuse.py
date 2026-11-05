from __future__ import annotations

import gzip
import json
import os
import sys
from pathlib import Path
from typing import Any

import pytest

ROOT = Path(__file__).resolve().parents[5]
BENCHMARK = ROOT / "src/agentsight/scripts/benchmark"
sys.path.insert(0, str(BENCHMARK / "single_run"))
sys.path.insert(0, str(BENCHMARK / "campaign"))

import campaign_evidence
import render_report

REPORT_METRICS = """timestamp,monotonic_timestamp,input_qps,cpu_pct,rss_mb,ring_buffer_dropped,channel_dropped,completed
1,1,20,10,30,0,0,100
2,2,20,20,40,2,0,110
"""
REPLACEMENT_REPORT_METRICS = """timestamp,monotonic_timestamp,input_qps,cpu_pct,rss_mb,ring_buffer_dropped,channel_dropped,completed
3,3,99,90,900,100,10,200
4,4,99,100,1000,110,20,300
"""
RECOVERY_METRICS = """timestamp,rss_mb,channel_length,connection_cache_bytes
0,130,5,50
1,105,2,20
2,104,2,20
3,103,2,20
"""
REPLACEMENT_RECOVERY_METRICS = """timestamp,rss_mb,channel_length,connection_cache_bytes
0,130,5,50
1,130,5,50
2,130,5,50
3,130,5,50
"""


def write_metrics(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.suffix == ".gz":
        with gzip.open(path, "wt", encoding="utf-8", newline="") as handle:
            handle.write(content)
    else:
        path.write_text(content, encoding="utf-8")


def track_metric_opens(
    monkeypatch: pytest.MonkeyPatch,
    path: Path,
    replacement: str | None = None,
) -> Any:
    owner = gzip if path.suffix == ".gz" else Path
    original_open = owner.open
    open_count = 0

    def tracked_open(file: Any, *args: Any, **kwargs: Any) -> Any:
        nonlocal open_count
        handle = original_open(file, *args, **kwargs)
        if Path(file) == path:
            open_count += 1
            if open_count == 1 and replacement is not None:
                replacement_path = path.with_name("replacement-" + path.name)
                write_metrics(replacement_path, replacement)
                os.replace(replacement_path, path)
        return handle

    monkeypatch.setattr(owner, "open", tracked_open)
    return lambda: open_count


def run_report(monkeypatch: pytest.MonkeyPatch, metrics: Path) -> dict[str, Any]:
    rendered: dict[str, Any] = {}
    render_markdown = render_report.render_markdown

    def capture_report(**kwargs: Any) -> str:
        rendered.update(kwargs)
        return render_markdown(**kwargs)

    monkeypatch.setattr(render_report, "render_markdown", capture_report)
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "render_report.py",
            "--output",
            str(metrics.parent / "report.md"),
            "--summary-output",
            str(metrics.parent / "summary.json"),
            "--protocol",
            "sse",
            "--qps",
            "20",
            "--duration",
            "2",
            "--metrics",
            str(metrics),
        ],
    )
    assert render_report.main() == 0
    summary = json.loads((metrics.parent / "summary.json").read_text(encoding="utf-8"))
    assert summary["schema_version"] == 1
    assert summary["resources"] == rendered["resources"]
    return rendered


def test_report_main_reads_one_plain_snapshot(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    metrics = tmp_path / "metrics.csv"
    write_metrics(metrics, REPORT_METRICS)
    open_count = track_metric_opens(monkeypatch, metrics)

    run_report(monkeypatch, metrics)

    assert open_count() == 1


def test_report_main_reads_one_gzip_snapshot(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    metrics = tmp_path / "metrics.csv.gz"
    write_metrics(metrics, REPORT_METRICS)
    open_count = track_metric_opens(monkeypatch, metrics)

    run_report(monkeypatch, metrics)

    assert open_count() == 1


@pytest.mark.parametrize("compressed", [False, True], ids=["plain", "gzip"])
def test_report_main_keeps_one_snapshot_after_replacement(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, compressed: bool
) -> None:
    filename = "metrics.csv.gz" if compressed else "metrics.csv"
    metrics = tmp_path / filename
    write_metrics(metrics, REPORT_METRICS)
    open_count = track_metric_opens(monkeypatch, metrics, replacement=REPLACEMENT_REPORT_METRICS)

    report = run_report(monkeypatch, metrics)

    assert open_count() == 1
    assert report["resources"]["rss_mb"]["avg"] == 35.0
    assert report["drops"]["ring_buffer_dropped"] == 2
    assert report["drops"]["completed"] == 10
    assert set(report["qps_resources"]) == {"20"}
    assert report["qps_resources"]["20"]["rss_mb"]["avg"] == 35.0
    markdown = (tmp_path / "report.md").read_text(encoding="utf-8")
    assert "| 20 | 2 | 15.00% |" in markdown
    assert "| 99 |" not in markdown


def write_recovery_inputs(run_result: Path, compressed: bool) -> Path:
    measurement = run_result.parent / "measurement"
    metrics = measurement / ("metrics.csv.gz" if compressed else "metrics.csv")
    write_metrics(metrics, RECOVERY_METRICS)
    records = []
    for second in range(4):
        records.extend(
            [
                {"metric": "benchmark_requests", "data": {"time": str(second), "value": 1}},
                {
                    "metric": "benchmark_latency",
                    "data": {"time": str(second), "value": 20 if second == 0 else 10},
                },
                {"metric": "benchmark_requests", "data": {"time": str(second), "value": 1}},
            ]
        )
    with gzip.open(measurement / "k6.jsonl.gz", "wt", encoding="utf-8") as handle:
        handle.write("\n".join(json.dumps(record) for record in records) + "\n")
    return metrics


def recovery_phases(tmp_path: Path) -> dict[str, tuple[Path, dict[str, Any]]]:
    phase_summary = {
        "effective_qps": 2.0,
        "latency_ms": {"p99": 10.0},
        "resources": {
            "rss_mb": {"avg": 100.0},
            "channel_length": {"avg": 2.0},
            "connection_cache_bytes": {"avg": 20.0},
        },
        "trace_completeness": 1.0,
        "process_survived": True,
        "runtime_clean": True,
    }
    phases = {}
    for label in campaign_evidence.RECOVERY_PHASES:
        run_result = tmp_path / label / "run-result.json"
        phases[label] = (
            run_result,
            {
                "summary": phase_summary,
                "evaluation": {"verdict": "FAIL" if label == "overload" else "PASS"},
                "started_at_unix": 0,
            },
        )
    return phases


@pytest.mark.parametrize("compressed", [False, True], ids=["plain", "gzip"])
def test_recovery_outcome_reads_metrics_once_from_one_snapshot(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, compressed: bool
) -> None:
    phases = recovery_phases(tmp_path)
    metrics = write_recovery_inputs(phases["recover"][0], compressed)
    open_count = track_metric_opens(monkeypatch, metrics, replacement=REPLACEMENT_RECOVERY_METRICS)

    outcome = campaign_evidence.recovery_outcome(
        phases,
        {"tolerance_ratio": 0.1, "recovery_window_seconds": 2},
        {"max_recovery_seconds": 10, "min_trace_completeness": 0.999},
    )

    assert open_count() == 1
    assert outcome["verdict"] == "PASS"
    assert outcome["seconds"] == {
        "effective_qps": 0.0,
        "latency_p99_ms": 1.0,
        "rss_mb": 1.0,
        "channel_length": 1.0,
        "connection_cache_bytes": 1.0,
    }


@pytest.mark.parametrize("compressed", [False, True], ids=["plain", "gzip"])
def test_report_snapshot_matches_standalone_summaries(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, compressed: bool
) -> None:
    metrics = tmp_path / ("metrics.csv.gz" if compressed else "metrics.csv")
    write_metrics(metrics, REPORT_METRICS)

    report = run_report(monkeypatch, metrics)

    assert report["resources"] == render_report.summarize_resources(metrics)
    assert report["drops"] == render_report.summarize_drops(metrics, 0)
    assert report["qps_resources"] == render_report.summarize_qps_resources(metrics)


@pytest.mark.parametrize("compressed", [False, True], ids=["plain", "gzip"])
@pytest.mark.parametrize("invalid_time", [False, True])
def test_recovery_snapshot_skips_unusable_samples_per_field(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, compressed: bool, invalid_time: bool
) -> None:
    phases = recovery_phases(tmp_path)
    metrics = write_recovery_inputs(phases["recover"][0], compressed)
    write_metrics(
        metrics,
        "timestamp,rss_mb,channel_length,connection_cache_bytes\n"
        + ("invalid,100,2,20\n" if invalid_time else "")
        + "0,nan,2,50\n"
        "1,105,2,inf\n"
        "2,104,2,20\n"
        "3,103,2,20\n"
        "4,102,2,20\n",
    )
    open_count = track_metric_opens(monkeypatch, metrics)

    outcome = campaign_evidence.recovery_outcome(
        phases,
        {"tolerance_ratio": 0.1, "recovery_window_seconds": 2},
        {"max_recovery_seconds": 10, "min_trace_completeness": 0.999},
    )

    assert open_count() == 1
    if invalid_time:
        assert outcome["verdict"] == "INCONCLUSIVE"
        assert outcome["missing"] == ["rss_mb"]
        assert outcome["seconds"]["rss_mb"] is None
        return
    assert outcome["verdict"] == "PASS"
    assert outcome["seconds"] == {
        "effective_qps": 0.0,
        "latency_p99_ms": 1.0,
        "rss_mb": 1.0,
        "channel_length": 0.0,
        "connection_cache_bytes": 2.0,
    }


@pytest.mark.parametrize("compressed", [False, True], ids=["plain", "gzip"])
@pytest.mark.parametrize("artifact", ["missing", "empty", "header"])
def test_empty_metric_artifacts_keep_missing_evidence(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    compressed: bool,
    artifact: str,
) -> None:
    phases = recovery_phases(tmp_path)
    metrics = write_recovery_inputs(phases["recover"][0], compressed)
    if artifact == "missing":
        metrics.unlink()
    else:
        content = (
            "timestamp,rss_mb,channel_length,connection_cache_bytes\n"
            if artifact == "header"
            else ""
        )
        write_metrics(metrics, content)

    report = run_report(monkeypatch, metrics)
    assert report["qps_resources"] == {}
    assert report["drops"]["total_dropped"] is None
    assert report["resources"]["rss_mb"]["avg"] is None
    assert campaign_evidence.resource_samples(phases["recover"][0], "rss_mb") == []

    outcome = campaign_evidence.recovery_outcome(
        phases,
        {"tolerance_ratio": 0.1, "recovery_window_seconds": 2},
        {"max_recovery_seconds": 10, "min_trace_completeness": 0.999},
    )
    assert outcome["verdict"] == "INCONCLUSIVE"
    assert outcome["missing"] == ["rss_mb"]
    assert outcome["failed"] == []
    assert set(outcome["seconds"]) == {"effective_qps", "latency_p99_ms", "rss_mb"}
