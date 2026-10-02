#!/usr/bin/env python3
"""Render benchmark artifacts as a compact Markdown report."""

from __future__ import annotations

import argparse
import csv
import gzip
import json
from collections.abc import Iterable
from itertools import pairwise
from pathlib import Path
from typing import Any

from benchmark_stats import linear_slope, numeric, rolling_max_increase, stats
from h2load_stats import summarize_h2load

LOAD_METRICS = {
    "benchmark_latency": "custom latency (ms)",
    "http_req_duration": "HTTP request duration (ms)",
}
RESOURCE_METRICS = (
    "process_alive",
    "cpu_pct",
    "rss_mb",
    "threads",
    "file_descriptors",
    "active_connections",
    "connection_cache_bytes",
    "channel_length",
    "event_channel_bytes",
    "event_channel_budget_bytes",
    "pending_genai_count",
    "pending_genai_bytes",
    "pending_connection_count",
    "pending_connection_bytes",
    "eviction_count",
    "ring_buffer_dropped",
    "channel_dropped",
    "completed",
)
QPS_RESOURCE_METRICS = ("cpu_pct", "rss_mb")
DROP_METRICS = ("ring_buffer_dropped", "channel_dropped", "completed")
REGRESSION_CHECKS = (
    (
        "Baseline and load",
        "benchmark/tests/test_benchmark.py::test_runner_shell_contract_is_valid",
    ),
    (
        "Bounded memory",
        (
            "probes::tests::sustained_load_never_exceeds_byte_budget; "
            "probes::tests::concurrent_drain_leaves_no_phantom_reservation"
        ),
    ),
    (
        "Bad input and cleanup",
        (
            "parser::http::request::tests::test_decode_chunked_json_binary_body_does_not_panic; "
            "aggregator::http::tests::test_oversized_request_body_pending_is_evicted"
        ),
    ),
)


def json_lines(path: Path) -> Iterable[dict[str, Any]]:
    """Yield valid JSON objects from a line-oriented k6 output file."""
    opener = gzip.open if path.suffix == ".gz" else Path.open
    with opener(path, mode="rt", encoding="utf-8") as handle:
        for line in handle:
            try:
                item = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(item, dict):
                yield item


def summarize_load(path: Path | None) -> dict[str, Any]:
    """Summarize counters and latency points from k6 JSON output."""
    result: dict[str, Any] = {
        "available": False,
        "requests": 0,
        "http_success": 0,
        "timeouts": 0,
        "throughput": None,
        "latency": stats([]),
        "latency_metric": "benchmark_latency",
        "latency_label": LOAD_METRICS["benchmark_latency"],
    }
    if path is None or not path.exists():
        return result
    result["available"] = True

    latency: dict[str, list[float]] = {name: [] for name in LOAD_METRICS}
    for item in json_lines(path):
        metric = item.get("metric")
        data = item.get("data")
        if not isinstance(data, dict):
            continue
        value = numeric(data.get("value"))
        if metric == "benchmark_requests" and value is not None:
            result["requests"] += int(value)
        elif metric == "benchmark_http_success" and value is not None:
            result["http_success"] += int(value)
        elif metric == "benchmark_timeouts" and value is not None:
            result["timeouts"] += int(value)
        elif metric in LOAD_METRICS and value is not None:
            latency[metric].append(value)

    chosen = (
        "benchmark_latency" if latency["benchmark_latency"] else "http_req_duration"
    )
    result["latency_metric"] = chosen
    result["latency_label"] = LOAD_METRICS[chosen]
    result["latency"] = stats(latency[chosen])
    return result


def summarize_resources(path: Path | None) -> dict[str, dict[str, float | None]]:
    """Summarize numeric columns from the process metrics CSV."""
    samples: dict[str, list[tuple[float, float]]] = {
        name: [] for name in RESOURCE_METRICS
    }
    for row in metric_rows(path):
        timestamp = numeric(row.get("timestamp"))
        if timestamp is None:
            continue
        for name in RESOURCE_METRICS:
            value = numeric(row.get(name))
            if value is not None:
                samples[name].append((timestamp, value))
    result: dict[str, dict[str, float | None]] = {}
    for name, metric_samples in samples.items():
        summary = stats([value for _, value in metric_samples])
        summary["slope_per_hour"] = linear_slope(metric_samples)
        summary["rolling_5m_max_increase"] = rolling_max_increase(metric_samples)
        result[name] = summary
    return result


def metric_rows(path: Path | None) -> list[dict[str, str]]:
    """Read process metric rows, returning no rows for a missing artifact."""
    if path is None or not path.exists():
        return []
    with path.open(newline="", encoding="utf-8") as handle:
        return list(csv.DictReader(handle))


def qps_key(value: Any) -> str:
    """Format the input QPS value used as a report table key."""
    number = numeric(value)
    if number is None:
        return "unknown"
    return str(int(number)) if number.is_integer() else f"{number:g}"


def qps_sort_key(value: str) -> tuple[bool, float, str]:
    """Sort numeric QPS values before rows with an unknown QPS."""
    number = numeric(value)
    return (number is None, number if number is not None else 0, value)


def summarize_qps_resources(path: Path | None) -> dict[str, Any]:
    """Group CPU and RSS samples by the input QPS recorded in the CSV."""
    grouped: dict[str, dict[str, Any]] = {}
    for row in metric_rows(path):
        key = qps_key(row.get("input_qps"))
        group = grouped.setdefault(key, {"samples": 0, "cpu_pct": [], "rss_mb": []})
        group["samples"] += 1
        for name in QPS_RESOURCE_METRICS:
            value = numeric(row.get(name))
            if value is not None:
                group[name].append(value)
    return {
        key: {
            "samples": group["samples"],
            "cpu_pct": stats(group["cpu_pct"]),
            "rss_mb": stats(group["rss_mb"]),
        }
        for key, group in grouped.items()
    }


def counter_delta(values: list[float]) -> int:
    """Return the increase in a cumulative counter, tolerating counter resets."""
    if len(values) < 2:
        return 0
    total = 0.0
    for previous, current in pairwise(values):
        total += max(0.0, current - previous)
    return int(total)


def summarize_drops(
    path: Path | None, load_requests: int
) -> dict[str, int | float | None]:
    """Summarize dropped events and derive a measurable drop rate."""
    values = {name: [] for name in DROP_METRICS}
    for row in metric_rows(path):
        for name in DROP_METRICS:
            value = numeric(row.get(name))
            if value is not None:
                values[name].append(value)
    ring_dropped = counter_delta(values["ring_buffer_dropped"])
    channel_dropped = counter_delta(values["channel_dropped"])
    completed = counter_delta(values["completed"])
    total_dropped = ring_dropped + channel_dropped
    available = any(
        len(values[name]) >= 2
        for name in ("ring_buffer_dropped", "channel_dropped")
    )
    denominator = completed + total_dropped or load_requests
    return {
        "ring_buffer_dropped": (
            ring_dropped if len(values["ring_buffer_dropped"]) >= 2 else None
        ),
        "channel_dropped": (
            channel_dropped if len(values["channel_dropped"]) >= 2 else None
        ),
        "completed": completed if len(values["completed"]) >= 2 else None,
        "total_dropped": (
            total_dropped if available else None
        ),
        "drop_rate": (
            total_dropped / denominator * 100 if available and denominator else None
        ),
    }


def build_summary(
    *,
    protocol: str,
    qps: str,
    duration: str,
    load: dict[str, Any],
    resources: dict[str, dict[str, float | None]],
    drops: dict[str, int | float | None],
    validation: dict[str, Any] | None,
) -> dict[str, Any]:
    """Build the machine-readable summary consumed by campaign reporting."""
    seconds = numeric(duration)
    requests = int(load["requests"])
    successes = int(load["http_success"])
    timeouts = int(load.get("timeouts", 0))
    alive = resources["process_alive"]
    internal_drop_rate = (
        drops["drop_rate"] / 100 if drops["drop_rate"] is not None else None
    )
    completeness = numeric(validation.get("completeness_ratio")) if validation else None
    expected_tokens = numeric(validation.get("expected_total_tokens")) if validation else None
    captured_tokens = numeric(validation.get("captured_total_tokens")) if validation else None
    capture_loss_rate = (
        max(0.0, 1.0 - completeness) if completeness is not None else None
    )
    observable_rates = [
        value for value in (internal_drop_rate, capture_loss_rate) if value is not None
    ]
    if internal_drop_rate is not None and capture_loss_rate is not None:
        drop_rate_source = "max(internal_counters,capture_reconciliation)"
    elif internal_drop_rate is not None:
        drop_rate_source = "internal_counters"
    elif capture_loss_rate is not None:
        drop_rate_source = "capture_reconciliation"
    else:
        drop_rate_source = None
    return {
        "schema_version": 1,
        "protocol": protocol,
        "input_qps": numeric(qps),
        "measured_seconds": seconds,
        "sent_requests": requests,
        "successful_http_requests": successes,
        "effective_qps": (
            load.get("throughput")
            if load.get("throughput") is not None
            else requests / seconds if seconds and seconds > 0 else None
        ),
        "tokens_per_second": {
            "generated": (
                expected_tokens / seconds
                if expected_tokens is not None and seconds and seconds > 0
                else None
            ),
            "captured": (
                captured_tokens / seconds
                if captured_tokens is not None and seconds and seconds > 0
                else None
            ),
        },
        "http_success_rate": successes / requests if requests else None,
        "http_error_rate": (requests - successes) / requests if requests else None,
        "timeout_rate": timeouts / requests if requests else None,
        "latency_ms": load["latency"],
        "resources": resources,
        "drop_rate": max(observable_rates) if observable_rates else None,
        "drop_rate_source": drop_rate_source,
        "capture_loss_rate": capture_loss_rate,
        "trace_match_rate": validation.get("match_ratio") if validation else None,
        "trace_completeness": completeness,
        "token_accuracy": validation.get("token_accuracy") if validation else None,
        "process_survived": (
            alive["min"] >= 1 if alive.get("min") is not None else None
        ),
    }


def display(value: Any, suffix: str = "") -> str:
    """Format report values without exposing Python's None spelling."""
    if value is None:
        return "—"
    if isinstance(value, float):
        return f"{value:.2f}{suffix}"
    return f"{value}{suffix}"


def render_markdown(
    *,
    protocol: str,
    qps: str,
    duration: str,
    load: dict[str, Any],
    resources: dict[str, dict[str, float | None]],
    qps_resources: dict[str, Any],
    drops: dict[str, int | float | None],
    validation: dict[str, Any] | None,
    load_artifact: Path | None,
    load_log_artifact: Path | None,
    metrics_artifact: Path | None,
) -> str:
    """Build a human-readable report from benchmark summaries."""
    completeness = numeric(validation.get("completeness_ratio")) if validation else None
    expected_tokens = numeric(validation.get("expected_total_tokens")) if validation else None
    captured_tokens = numeric(validation.get("captured_total_tokens")) if validation else None
    capture_loss_percent = (
        max(0.0, 1.0 - completeness) * 100 if completeness is not None else None
    )
    lines = [
        "# AgentSight benchmark report",
        "",
        "## Run configuration",
        "",
        "| Protocol | Target QPS | Duration |",
        "| --- | ---: | ---: |",
        f"| {protocol} | {qps} | {duration}s |",
        "",
        "## Load results",
        "",
        (
            "| Requests | HTTP success | HTTP errors | Timeouts | Success rate | Throughput | "
            "Latency metric | Avg | P95 | P99 | Max |"
        ),
        "| ---: | ---: | ---: | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: |",
    ]
    requests = load["requests"]
    success = load["http_success"]
    timeouts = int(load.get("timeouts", 0))
    success_rate = success / requests * 100 if requests else None
    seconds = numeric(duration)
    throughput = (
        load.get("throughput")
        if load.get("throughput") is not None
        else requests / seconds if seconds and seconds > 0 else None
    )
    latency = load["latency"]
    if load["available"]:
        lines.append(
            f"| {requests} | {success} | {requests - success} | {timeouts} | "
            f"{display(success_rate, '%')} | "
            f"{display(throughput, ' req/s')} | "
            f"{load['latency_label']} | {display(latency['avg'], ' ms')} | "
            f"{display(latency['p95'], ' ms')} | {display(latency['p99'], ' ms')} | "
            f"{display(latency['max'], ' ms')} |"
        )
    else:
        lines.append("| — | — | — | — | — | — | No load summary | — | — | — | — |")
    generated_tps = (
        expected_tokens / seconds
        if expected_tokens is not None and seconds is not None and seconds > 0
        else None
    )
    captured_tps = (
        captured_tokens / seconds
        if captured_tokens is not None and seconds is not None and seconds > 0
        else None
    )
    lines.extend(
        [
            "",
            "## Token throughput",
            "",
            "| Generated tokens/s | Captured tokens/s |",
            "| ---: | ---: |",
            f"| {display(generated_tps)} | {display(captured_tps)} |",
        ]
    )
    lines.extend(
        [
            "",
            "## QPS to CPU and memory",
            "",
            "| Input QPS | Samples | CPU Avg | CPU P95 | RSS Avg | RSS P99 | RSS Max |",
            "| ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
        ]
    )
    for qps_value in sorted(qps_resources, key=qps_sort_key):
        group = qps_resources[qps_value]
        cpu = group["cpu_pct"]
        rss = group["rss_mb"]
        lines.append(
            f"| {qps_value} | {group['samples']} | {display(cpu['avg'], '%')} | "
            f"{display(cpu['p95'], '%')} | "
            f"{display(rss['avg'], ' MB')} | {display(rss['p99'], ' MB')} | "
            f"{display(rss['max'], ' MB')} |"
        )
    if not qps_resources:
        lines.append("| — | — | — | — | — | — | — |")
        lines.append(
            "No process metrics collected; pass `--agentsight-pid PID` to record CPU and RSS."
        )

    lines.extend(
        [
            "",
            "## AgentSight resource samples",
            "",
            "| Metric | First | Last | Avg | P95 | P99 | Max | Slope/hour |",
            "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
        ]
    )
    units = {
        "cpu_pct": "%",
        "rss_mb": " MB",
        "connection_cache_bytes": " bytes",
        "event_channel_bytes": " bytes",
        "event_channel_budget_bytes": " bytes",
        "pending_genai_bytes": " bytes",
        "pending_connection_bytes": " bytes",
    }
    resource_row_added = False
    for name, values in resources.items():
        if all(value is None for value in values.values()):
            continue
        resource_row_added = True
        suffix = units.get(name, "")
        lines.append(
            f"| {name} | {display(values['first'], suffix)} | "
            f"{display(values['last'], suffix)} | {display(values['avg'], suffix)} | "
            f"{display(values['p95'], suffix)} | {display(values['p99'], suffix)} | "
            f"{display(values['max'], suffix)} | "
            f"{display(values['slope_per_hour'], f'{suffix}/h')} |"
        )
    if not resource_row_added:
        lines.append(
            "| No additional process metrics collected | — | — | — | — | — | — | — |"
        )

    lines.extend(
        [
            "",
            "## Drop rate",
            "",
            "| Ring-buffer dropped | Channel dropped | Completed | Total dropped | Internal drop rate | End-to-end capture loss |",
            "| ---: | ---: | ---: | ---: | ---: | ---: |",
            (
                f"| {display(drops['ring_buffer_dropped'])} | "
                f"{display(drops['channel_dropped'])} | {display(drops['completed'])} | "
                f"{display(drops['total_dropped'])} | {display(drops['drop_rate'], '%')} | "
                f"{display(capture_loss_percent, '%')} |"
            ),
        ]
    )
    if drops["drop_rate"] is None and validation is not None:
        lines.append(
            "Internal drop counters are unavailable; the campaign gate uses "
            "end-to-end request-ID reconciliation instead."
        )

    lines.extend(["", "## Capture validation", ""])
    if validation is None:
        lines.append("No SQLite capture validation was requested.")
    else:
        lines.extend(
            [
                "| Matched | Complete | Completeness | Token-correct |",
                "| ---: | ---: | ---: | ---: |",
                (
                    f"| {validation.get('matched', '—')} | "
                    f"{validation.get('complete', '—')} | "
                    f"{display(completeness * 100 if completeness is not None else None, '%')} | "
                    f"{validation.get('token_correct', '—')} |"
                ),
            ]
        )
    lines.extend(
        [
            "",
            "## Regression coverage",
            "",
            "| Area | Checks |",
            "| --- | --- |",
        ]
    )
    for area, checks in REGRESSION_CHECKS:
        lines.append(f"| {area} | `{checks}` |")
    lines.extend(["", "## Raw artifacts", ""])
    if load_artifact:
        lines.append(f"- Load output: `{load_artifact.name}`")
    if load_log_artifact:
        lines.append(f"- Load log: `{load_log_artifact.name}`")
    if metrics_artifact:
        lines.append(f"- Process metrics: `{metrics_artifact.name}`")
    lines.append(
        "- This report is intended for quick inspection; use the raw artifacts "
        "for detailed analysis."
    )
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--protocol", required=True)
    parser.add_argument("--qps", required=True)
    parser.add_argument("--duration", required=True)
    parser.add_argument("--load-results", type=Path)
    parser.add_argument("--load-log", type=Path)
    parser.add_argument("--metrics", type=Path)
    parser.add_argument("--validation-report", type=Path)
    parser.add_argument("--summary-output", type=Path)
    args = parser.parse_args()
    validation = None
    if args.validation_report and args.validation_report.exists():
        try:
            loaded_validation = json.loads(
                args.validation_report.read_text(encoding="utf-8")
            )
        except json.JSONDecodeError:
            loaded_validation = None
        if isinstance(loaded_validation, dict):
            validation = loaded_validation
    load = (
        summarize_h2load(args.load_log)
        if args.protocol == "h2"
        else summarize_load(args.load_results)
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    resources = summarize_resources(args.metrics)
    drops = summarize_drops(args.metrics, load["requests"])
    args.output.write_text(
        render_markdown(
            protocol=args.protocol,
            qps=args.qps,
            duration=args.duration,
            load=load,
            resources=resources,
            qps_resources=summarize_qps_resources(args.metrics),
            drops=drops,
            validation=validation,
            load_artifact=args.load_results,
            load_log_artifact=args.load_log,
            metrics_artifact=args.metrics,
        ),
        encoding="utf-8",
    )
    if args.summary_output:
        args.summary_output.parent.mkdir(parents=True, exist_ok=True)
        summary = build_summary(
            protocol=args.protocol,
            qps=args.qps,
            duration=args.duration,
            load=load,
            resources=resources,
            drops=drops,
            validation=validation,
        )
        args.summary_output.write_text(
            json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
    print(f"human-readable report: {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
