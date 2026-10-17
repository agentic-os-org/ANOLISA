"""Report load-window RSS and CPU, with separate response-drain measurements."""

from __future__ import annotations

import csv
import json
import math
from pathlib import Path
from typing import Any

from benchmark_stats import numeric, percentile


def read_json(path: Path) -> dict[str, Any]:
    """Read an optional completed artifact; absent artifacts remain unverified."""
    return json.loads(path.read_text()) if path.exists() else {}


def resource_stats(rows: list[dict[str, str]], start: float, end: float) -> dict[str, Any]:
    """Weight CPU by interval overlap; RSS peaks remain sampled point measurements."""
    rss: list[float] = []
    cpu: list[float] = []
    cpu_seconds = covered = 0.0
    complete = True
    for index, row in enumerate(rows):
        timestamp = numeric(row.get("monotonic_timestamp"))
        if timestamp is None:
            complete = False
            continue
        value = numeric(row.get("rss_mb"))
        if start <= timestamp < end and value is not None and value >= 0:
            rss.append(value)
        interval = numeric(row.get("cpu_interval_seconds"))
        if interval is None or interval <= 0:
            # The initial RSS sample establishes the CPU baseline and has no interval.
            if index != 0 or row.get("cpu_pct") or row.get("cpu_interval_seconds"):
                complete = False
            continue
        overlap = min(timestamp, end) - max(timestamp - interval, start)
        if overlap <= 0:
            continue
        value = numeric(row.get("cpu_pct"))
        if value is None or value < 0:
            complete = False
            continue
        cpu.append(value)
        cpu_seconds += value / 100 * overlap
        covered += overlap
    return {
        "rss_average_mib": sum(rss) / len(rss) if rss else None,
        "rss_peak_mib": max(rss) if rss else None,
        "samples": len(rss),
        "cpu_average_pct": cpu_seconds / covered * 100 if covered else None,
        "cpu_p95_pct": percentile(cpu),
        "cpu_peak_pct": max(cpu) if cpu else None,
        "cpu_seconds": cpu_seconds if cpu else None,
        "cpu_samples": len(cpu),
        "cpu_measurement_seconds": covered,
        "cpu_samples_valid": complete and bool(cpu),
    }


def summarize(directory: Path, source: dict[str, Any]) -> dict[str, Any]:
    """Gate load-window resources on both capture correctness and usable CPU samples."""
    worker = read_json(directory / "worker-result.json")
    load = read_json(directory / "agent-summary.json")
    capture = read_json(directory / "report.json")
    safety_stop = read_json(directory / "safety-stop.json")
    rows: list[dict[str, str]] = []
    evictions: list[float] = []
    alive: list[bool] = []
    drop_samples: dict[str, list[float]] = {
        "ring_buffer_dropped": [],
        "channel_dropped": [],
    }
    metrics = directory / "metrics.csv"
    if metrics.exists():
        with metrics.open(newline="") as handle:
            rows = list(csv.DictReader(handle))
            for row in rows:
                alive.append(row.get("process_alive") == "1")
                if row.get("eviction_count"):
                    value = float(row["eviction_count"])
                    if math.isfinite(value) and value >= 0:
                        evictions.append(value)
                for key, values in drop_samples.items():
                    if row.get(key):
                        value = float(row[key])
                        if math.isfinite(value) and value >= 0:
                            values.append(value)
    window = read_json(directory / "agents/start.json")
    start, end = numeric(window.get("start")), numeric(window.get("end"))
    window_valid = start is not None and end is not None and end > start
    resources = resource_stats(rows, start, end) if window_valid else resource_stats([], 0, 0)
    tail = resource_stats(rows, end, math.inf) if window_valid else None
    average, peak = resources["rss_average_mib"], resources["rss_peak_mib"]
    resource_error = None
    if not window_valid:
        resource_error = "missing or invalid load measurement window"
    elif not resources["samples"] or not resources["cpu_samples_valid"]:
        resource_error = "missing or invalid RSS/CPU samples in the load window"
    elif resources["cpu_measurement_seconds"] < (end - start) * 0.95:
        resource_error = "CPU samples cover less than 95% of the load window"
    total = worker.get("host_memory_mib")
    load_status = "PASS" if load.get("passed") is True else "FAIL" if load else "UNVERIFIED"
    probe_drops = {key: max(values) if values else None for key, values in drop_samples.items()}
    events_dropped = any(value is not None and value > 0 for value in probe_drops.values())
    capture_ok = (
        capture.get("sent", 0) > 0
        and capture.get("completeness_ratio", 0) >= 0.999
        and (capture.get("token_accuracy") or 0) >= 0.999
        and not events_dropped
    )
    capture_status = (
        "FAIL"
        if worker.get("capture_ready") is False
        else "PASS" if capture_ok else "FAIL" if capture or events_dropped else "UNVERIFIED"
    )
    valid = (
        worker.get("status") == "complete"
        and worker.get("returncode") == 0
        and worker.get("trace_survived") is True
        and resource_error is None
        and alive
        and all(alive)
        and load_status == capture_status == "PASS"
        and not safety_stop
    )
    capture_counts = {
        key: capture.get(key)
        for key in ("sent", "matched", "complete", "missing_count", "token_correct")
    }
    if capture_counts["missing_count"] is None and isinstance(capture.get("missing_ids"), list):
        capture_counts["missing_count"] = len(capture["missing_ids"])
    return {
        "source": source,
        "status": worker.get("status", "not-run"),
        "error": worker.get("error"),
        "valid": bool(valid),
        **resources,
        "measurement_window": window,
        "tail": tail,
        "resource_error": resource_error,
        "logical_cpus": worker.get("logical_cpus"),
        "host_memory_mib": total,
        "rss_average_host_pct": average / total * 100 if average is not None and total else None,
        "rss_peak_host_pct": peak / total * 100 if peak is not None and total else None,
        "load": load_status,
        "capture": capture_status,
        "capture_completeness": capture.get("completeness_ratio"),
        "capture_token_accuracy": capture.get("token_accuracy"),
        "capture_counts": capture_counts,
        "connection_evictions": max(evictions) if evictions else None,
        "probe_drops": probe_drops,
        "safety_stop": safety_stop or None,
    }


def safety_message(record: dict[str, Any]) -> str:
    """Describe each guard with the units used by its command-line threshold."""
    reason = record.get("reason", "unknown")
    dimensions = {
        "available_memory": ("host available RAM", "<", 1024**2, "MiB"),
        "agentsight_rss": ("AgentSight RSS", ">", 1, "MiB"),
        "free_disk": ("free disk", "<", 1024**3, "GiB"),
        "results_budget": ("results size", ">=", 1024**3, "GiB"),
    }
    observed, limit = record.get("observed"), record.get("limit")
    if reason not in dimensions or not all(
        isinstance(value, (int, float)) for value in (observed, limit)
    ):
        return str(reason)
    title, operator, divisor, unit = dimensions[reason]
    return f"{title} {observed / divisor:.2f} {unit} {operator} {limit / divisor:.2f} {unit}"


def reduction(before: float, after: float) -> dict[str, float | None]:
    """Positive reductions indicate less memory; zero baselines have no percentage."""
    return {"mib": before - after, "percent": (before - after) / before * 100 if before else None}


def report(output: Path, sources: dict[str, dict[str, Any]]) -> bool:
    """Print and persist a plain text report with a machine-readable companion."""
    runs = {label: summarize(output / label, source) for label, source in sources.items()}
    error_file = output / "run-error.txt"
    valid = bool(runs) and all(run["valid"] for run in runs.values()) and not error_file.exists()
    comparison: dict[str, Any] = {}
    if valid and len(runs) == 2:
        for key in ("rss_average_mib", "rss_peak_mib"):
            comparison[key] = reduction(runs["before"][key], runs["after"][key])
        for key in ("rss_average_host_pct", "rss_peak_host_pct"):
            comparison[key] = {"percentage_points": runs["before"][key] - runs["after"][key]}
        for key in ("cpu_average_pct", "cpu_p95_pct", "cpu_peak_pct"):
            before, after = runs["before"][key], runs["after"][key]
            comparison[key] = {
                "percentage_points": before - after,
                "percent": (before - after) / before * 100 if before else None,
            }

    def display(value: float | None, suffix: str = "") -> str:
        return f"{value:.2f}{suffix}" if value is not None else "N/A"

    def drop_count(value: float | None) -> str:
        return f"{value:.0f}" if value is not None else "N/A"

    lines = [
        "AgentSight resource result (process RSS and CPU; MiB = 1,048,576 bytes)",
        "CPU: 100% = one logical CPU; all process threads; values may exceed 100%.",
        "Main statistics cover the load window; response drain is reported separately.",
        "CPU average is time-weighted; CPU P95/peak are interval samples, RSS peak is sampled.",
        "Intervals crossing the load boundary are split proportionally by overlap.",
    ]
    settings = read_json(output / "settings.json")
    if settings:
        lines.append(
            f"Load target: {settings['agents']} agents x {settings['tokens_per_second']:g} output tok/s, "
            f"{settings['duration']:g}s; {settings['tokens_per_response']} tokens/response, "
            f"{settings['payload_kb']} KiB request padding"
        )
        if "connection_capacity" in settings:
            lines.append(f"Configured connection capacity: {settings['connection_capacity']}")
        if settings.get("cpu_budget_percent"):
            lines.append(
                f"Configured CPU pacing target: {settings['cpu_budget_percent']}% of one logical CPU (cooperative)"
            )
        if "ring_buffer_mb" in settings:
            lines.append(f"Configured ring buffer: {settings['ring_buffer_mb']} MiB")
    for label, run in runs.items():
        source = run["source"]
        version = source.get("commit", source.get("path", "unknown"))
        if source.get("ref") == "working-tree":
            version += f" + working-tree (tree {source['tree']})"
        lines.append(f"{label}: {version}")
    ab = len(runs) == 2
    lines.append(
        f"{'Metric':<24}"
        + "".join(f"{label:>16}" for label in runs)
        + (f"{'Reduction':>28}" if ab else "")
    )
    for title, key, suffix in (
        ("Average RSS", "rss_average_mib", " MiB"),
        ("Peak RSS", "rss_peak_mib", " MiB"),
        ("Average / host RAM", "rss_average_host_pct", "%"),
        ("Peak / host RAM", "rss_peak_host_pct", "%"),
        ("Average CPU", "cpu_average_pct", "%"),
        ("P95 CPU", "cpu_p95_pct", "%"),
        ("Peak CPU", "cpu_peak_pct", "%"),
    ):
        change = comparison.get(key)
        delta = "N/A (invalid)"
        if change is not None:
            if key.endswith("_mib"):
                delta = f"{display(change['mib'])} MiB ({display(change['percent'], '%')})"
            else:
                delta = f"{display(change['percentage_points'])} pp"
                if "percent" in change:
                    delta += f" ({display(change['percent'], '%')})"
        lines.append(
            f"{title:<24}"
            + "".join(f"{display(run[key], suffix):>16}" for run in runs.values())
            + (f"{delta:>28}" if ab else "")
        )
    for title, key in (("Load", "load"), ("Capture", "capture"), ("Run status", "status")):
        lines.append(
            f"{title:<24}"
            + "".join(f"{run[key]:>16}" for run in runs.values())
            + (f"{'-':>28}" if ab else "")
        )
    for label, run in runs.items():
        lines.append(
            f"{label} CPU measurement: {run['cpu_samples']} samples, "
            f"{display(run['cpu_measurement_seconds'])} s, "
            f"{display(run['cpu_seconds'])} CPU s; logical CPUs: {run['logical_cpus']}"
        )
        if run["resource_error"]:
            lines.append(f"{label} resources: {run['resource_error']}")
        tail = run["tail"]
        if tail is not None:
            lines.append(
                f"{label} response drain: RSS avg/peak "
                f"{display(tail['rss_average_mib'])}/{display(tail['rss_peak_mib'])} MiB; "
                f"CPU avg/P95/peak {display(tail['cpu_average_pct'])}/"
                f"{display(tail['cpu_p95_pct'])}/{display(tail['cpu_peak_pct'])}%"
            )
        if run["safety_stop"]:
            lines.append(f"{label} stopped by resource guard: {safety_message(run['safety_stop'])}")
        drops = run["probe_drops"]
        lines.append(
            f"{label} probe drops (all probe types): "
            f"ring={drop_count(drops['ring_buffer_dropped'])}; "
            f"channel={drop_count(drops['channel_dropped'])}"
        )
        if any(value is not None and value > 0 for value in drops.values()):
            lines.append(f"{label} capture failed: probe events were dropped")
        if run["capture"] == "FAIL":
            counts = {
                key: str(value) if value is not None else "N/A"
                for key, value in run["capture_counts"].items()
            }
            lines.append(
                f"{label} capture: {counts['complete']}/{counts['sent']} complete; "
                f"{counts['missing_count']} missing; "
                f"{counts['token_correct']}/{counts['sent']} token-correct"
            )
            if run["connection_evictions"] is not None:
                lines.append(f"{label} connection evictions: {run['connection_evictions']:.0f}")
    lines.append(f"Result: {'VALID' if valid else 'INVALID; do not infer a resource improvement'}")
    if error_file.exists():
        lines.append(f"Error: {error_file.read_text().strip()}")
    for label, run in runs.items():
        if run["error"]:
            lines.append(f"{label} error: {run['error']}")
            for name in ("trace.log", "runner.log", "worker.log"):
                log = output / label / name
                if name in run["error"] and log.exists():
                    lines.extend(log.read_text(errors="replace").splitlines()[-4:])
    lines.append(f"Artifacts: {output}")
    text = "\n".join(lines) + "\n"
    (output / "resource-report.txt").write_text(text)
    (output / "resource-summary.json").write_text(
        json.dumps(
            {
                "valid": valid,
                "runs": runs,
                "comparison": comparison,
                "rss_unit": "MiB",
                "cpu_unit": "% of one logical CPU (100% = one logical CPU)",
                "cpu_average_method": "time-weighted by interval overlap with the load window",
                "comparison_order": list(runs),
                "settings": settings,
            },
            indent=2,
        )
        + "\n"
    )
    print(text, end="", flush=True)
    return valid
