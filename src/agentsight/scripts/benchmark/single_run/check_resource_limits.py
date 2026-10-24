"""Enforce the fixed CI resource profile and its process RSS/CPU limits."""

from __future__ import annotations

import argparse
import json
import math
import os
import sys
from pathlib import Path
from typing import Any

EXPECTED_SETTINGS = {
    "agents": 32,
    "connection_capacity": 128,
    "payload_kb": 1024,
    "tokens_per_second": 100,
    "tokens_per_response": 1000,
    "duration": 300,
    "ring_buffer_mb": 64,
    "cpu_budget_percent": 25,
    "mode": "cli",
}
LIMITS = {
    "rss_average_mib": ("Average RSS", 265, "MiB"),
    "rss_peak_mib": ("Peak RSS", 300, "MiB"),
    "cpu_average_pct": ("Average CPU", 23.5, "%"),
    "cpu_p95_pct": ("P95 CPU", 34, "%"),
    "cpu_peak_pct": ("Peak CPU", 42, "%"),
}


def evaluate(summary: Any) -> tuple[bool, str]:
    """Reject invalid evidence before accepting measurements within the limits."""
    if not isinstance(summary, dict):
        raise ValueError("resource summary must be a JSON object")
    runs = summary.get("runs")
    if not isinstance(runs, dict) or set(runs) != {"run"}:
        raise ValueError("resource summary must contain exactly one run named 'run'")
    run = runs["run"]
    settings = summary.get("settings")
    if not isinstance(run, dict) or not isinstance(settings, dict):
        raise ValueError("resource summary must contain run and settings objects")

    errors: list[str] = []
    if summary.get("valid") is not True or run.get("valid") is not True:
        errors.append("Benchmark is INVALID; inspect the load and capture reports.")
    if summary.get("rss_unit") != "MiB" or summary.get("cpu_unit") != (
        "% of one logical CPU (100% = one logical CPU)"
    ):
        errors.append("Resource measurement units are missing or incompatible.")
    for name, expected in EXPECTED_SETTINGS.items():
        actual = settings.get(name)
        if isinstance(actual, bool) or actual != expected:
            errors.append(f"Expected settings.{name} = {expected!r}, got {actual!r}.")
    window = run.get("measurement_window")
    if not isinstance(window, dict) or not all(
        type(window.get(key)) in (int, float) and math.isfinite(window[key])
        for key in ("start", "end")
    ):
        errors.append("Missing or invalid load measurement window.")
    elif not math.isclose(window["end"] - window["start"], 300, rel_tol=0, abs_tol=1e-6):
        errors.append("The measured load window must span 300 seconds.")

    lines = [
        "### AgentSight resource limits",
        "",
        "Load window: 300 seconds. CPU: 100% = one logical CPU.",
        "RSS peaks and CPU P95/peaks use approximately one-second samples.",
        "",
        "| Metric | Observed | Limit | Result |",
        "|---|---:|---:|---|",
    ]
    for key, (label, maximum, unit) in LIMITS.items():
        value = run.get(key)
        numeric = (
            type(value) in (int, float)
            and (not isinstance(value, float) or math.isfinite(value))
            and value >= 0
        )
        passed = numeric and value <= maximum
        observed = f"{value} {unit}" if numeric else "INVALID"
        lines.append(
            f"| {label} | {observed} | {maximum} {unit} | {'PASS' if passed else 'FAIL'} |"
        )
        if not numeric:
            errors.append(f"Missing or invalid metric: runs.run.{key}.")
        elif not passed:
            errors.append(f"{label} exceeds its limit: {value} {unit} > {maximum} {unit}.")
    lines.extend(["", *(f"- {error}" for error in errors)])
    lines.extend(["", f"Result: {'FAIL' if errors else 'PASS'}", ""])
    return not errors, "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    """Return a failing status for unavailable reports or exceeded resource limits."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--summary", type=Path, required=True)
    github_summary = os.environ.get("GITHUB_STEP_SUMMARY")
    parser.add_argument(
        "--github-summary", type=Path, default=Path(github_summary) if github_summary else None
    )
    args = parser.parse_args(argv)
    try:
        passed, text = evaluate(json.loads(args.summary.read_text(encoding="utf-8")))
    except (OSError, ValueError) as error:
        passed = False
        text = f"### AgentSight resource limits\n\nResult: FAIL\n\n{error}\n"
    print(text, end="")
    if args.github_summary is not None:
        try:
            with args.github_summary.open("a", encoding="utf-8") as handle:
                handle.write(text)
        except OSError as error:
            print(f"Cannot append GitHub job summary: {error}", file=sys.stderr)
            return 1
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
