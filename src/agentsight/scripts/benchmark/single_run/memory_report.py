"""Report measured RSS and gate comparisons on load and capture correctness."""

from __future__ import annotations

import csv
import json
import math
from pathlib import Path
from typing import Any


def read_json(path: Path) -> dict[str, Any]:
    """Read an optional completed artifact; absent artifacts remain unverified."""
    return json.loads(path.read_text()) if path.exists() else {}


def summarize(directory: Path, source: dict[str, Any]) -> dict[str, Any]:
    """Keep RSS in MiB and distinguish interrupted, failed and unverified runs."""
    worker = read_json(directory / "worker-result.json")
    load = read_json(directory / "agent-summary.json")
    capture = read_json(directory / "report.json")
    safety_stop = read_json(directory / "safety-stop.json")
    samples: list[float] = []
    evictions: list[float] = []
    alive: list[bool] = []
    drop_samples: dict[str, list[float]] = {
        "ring_buffer_dropped": [],
        "channel_dropped": [],
    }
    metrics = directory / "metrics.csv"
    if metrics.exists():
        with metrics.open(newline="") as handle:
            for row in csv.DictReader(handle):
                alive.append(row.get("process_alive") == "1")
                if row.get("rss_mb"):
                    value = float(row["rss_mb"])
                    if math.isfinite(value) and value >= 0:
                        samples.append(value)
                if row.get("eviction_count"):
                    value = float(row["eviction_count"])
                    if math.isfinite(value) and value >= 0:
                        evictions.append(value)
                for key, values in drop_samples.items():
                    if row.get(key):
                        value = float(row[key])
                        if math.isfinite(value) and value >= 0:
                            values.append(value)
    average = sum(samples) / len(samples) if samples else None
    peak = max(samples) if samples else None
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
        and samples
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
        "rss_average_mib": average,
        "rss_peak_mib": peak,
        "host_memory_mib": total,
        "rss_average_host_pct": average / total * 100 if average is not None and total else None,
        "rss_peak_host_pct": peak / total * 100 if peak is not None and total else None,
        "samples": len(samples),
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

    def display(value: float | None, suffix: str = "") -> str:
        return f"{value:.2f}{suffix}" if value is not None else "N/A"

    def drop_count(value: float | None) -> str:
        return f"{value:.0f}" if value is not None else "N/A"

    lines = ["AgentSight memory result (process RSS; MiB = 1,048,576 bytes)"]
    settings = read_json(output / "settings.json")
    if settings:
        lines.append(
            f"Load target: {settings['agents']} agents x {settings['tokens_per_second']:g} output tok/s, "
            f"{settings['duration']:g}s; {settings['tokens_per_response']} tokens/response, "
            f"{settings['payload_kb']} KiB request padding"
        )
        if "connection_capacity" in settings:
            lines.append(f"Configured connection capacity: {settings['connection_capacity']}")
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
    ):
        change = comparison.get(key)
        delta = "N/A (invalid)"
        if change is not None:
            delta = (
                f"{display(change['mib'])} MiB ({display(change['percent'], '%')})"
                if key.endswith("_mib")
                else f"{display(change['percentage_points'])} pp"
            )
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
    lines.append(f"Result: {'VALID' if valid else 'INVALID; do not infer a memory improvement'}")
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
    (output / "memory-report.txt").write_text(text)
    (output / "memory-summary.json").write_text(
        json.dumps(
            {
                "valid": valid,
                "runs": runs,
                "comparison": comparison,
                "rss_unit": "MiB",
                "comparison_order": list(runs),
                "settings": settings,
            },
            indent=2,
        )
        + "\n"
    )
    print(text, end="", flush=True)
    return valid
