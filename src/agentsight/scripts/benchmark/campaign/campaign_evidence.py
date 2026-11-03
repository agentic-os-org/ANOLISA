"""Evaluate campaign structure and raw recovery/fault evidence."""

from __future__ import annotations

import csv
import gzip
import json
import math
import shlex
import sys
from collections import defaultdict
from datetime import datetime
from pathlib import Path
from typing import Any

from campaign_evaluation import bounded_measurement

# Campaign evidence uses the same percentile calculation as a single run.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "single_run"))
from benchmark_stats import numeric, percentile
from fault_injector import delivery_failures
from validate_results import json_lines

VERSIONS = ("baseline", "optimized")
RECOVERY_PHASES = ("stable", "overload", "recover")
FAULT_CASES = {
    "invalid_json",
    "invalid_chunk_size",
    "content_length_mismatch",
    "truncated_body",
    "truncated_sse",
    "invalid_utf8",
    "binary_body",
    "oversized_body",
    "tls_connect_then_close",
}

REQUIRED_REGRESSION_TOOLS = ("diff-cover", "cargo fmt", "cargo clippy", "cargo test")


def regression_tool(check: dict[str, Any]) -> str | None:
    """Identify the executed tool, never tool names inside arguments or output."""
    try:
        command = shlex.split(check["command"])
    except ValueError:
        return None
    argv = check.get("argv", command)
    if not isinstance(argv, list) or not argv or argv != command:
        return None
    if not all(isinstance(argument, str) for argument in argv):
        return None
    if any(flag in argv for flag in ("--help", "-h", "--version", "-V", "--list", "--no-run")):
        return None
    if Path(argv[0]).name == "rustup":
        if len(argv) < 5 or argv[1] != "run":
            return None
        argv = argv[3:]
    executable = Path(argv[0]).name
    if executable == "cargo" and len(argv) >= 2:
        # Cargo also accepts an explicit +toolchain selector before the subcommand.
        position = 2 if argv[1].startswith("+") else 1
        if len(argv) > position and argv[position] in ("fmt", "clippy", "test"):
            return "cargo " + argv[position]
    if executable == "diff-cover":
        return "diff-cover"
    if executable.startswith("python") and argv[1:3] == ["-m", "diff_cover.diff_cover_tool"]:
        return "diff-cover"
    return None


def usable_regression_check(check: Any) -> bool:
    """Require the same command/exit-code contract in every regression report."""
    return (
        isinstance(check, dict)
        and isinstance(check.get("command"), str)
        and isinstance(check.get("exit_code"), int)
        and not isinstance(check.get("exit_code"), bool)
    )


def nested(value: dict[str, Any], *keys: str) -> float | bool | None:
    """Read a numeric or boolean value from nested dictionaries."""
    current: Any = value
    for key in keys:
        if not isinstance(current, dict):
            return None
        current = current.get(key)
    return current if isinstance(current, (int, float, bool)) else None


def meets(actual: float | bool, expected: float | bool) -> bool:
    """Apply minimum numeric thresholds and exact boolean gates."""
    if isinstance(expected, bool):
        return actual is expected
    return not isinstance(actual, bool) and actual >= expected


def finite_float(value: Any) -> float | None:
    """Convert a sample value to a finite float, skipping unusable data."""
    if isinstance(value, bool):
        return None
    try:
        result = float(value)
    except (TypeError, ValueError, OverflowError):
        return None
    return result if math.isfinite(result) else None


def timestamp(value: Any) -> float | None:
    """Parse a finite Unix or RFC 3339 timestamp from the metric collectors."""
    if isinstance(value, bool) or value is None:
        return None
    if isinstance(value, (int, float)):
        return finite_float(value)
    if not isinstance(value, str) or not value:
        return None
    result = finite_float(value)
    if result is not None:
        return result
    try:
        return finite_float(
            datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()
        )
    except ValueError:
        return None


def continuous_recovery(
    samples: list[tuple[float, float | None]],
    reference: float | None,
    tolerance: float,
    window_seconds: int,
    *,
    higher_is_better: bool,
    max_gap_seconds: float = 2.5,
    started_at: float | None = None,
    ended_at: float | None = None,
) -> float | None:
    """Return time to the first uninterrupted recovered window."""
    reference = bounded_measurement(reference)
    if reference is None or not samples:
        return None
    ordered = sorted(
        ((sample_time, bounded_measurement(value)) for sample_time, value in samples),
        key=lambda sample: sample[0],
    )
    origin = ordered[0][0] if started_at is None else started_at
    threshold = reference * (1 - tolerance if higher_is_better else 1 + tolerance)
    recovered_at: float | None = None
    previous: float | None = None
    for sample_time, value in ordered:
        # Legacy k6 series label each second at its beginning. Raw load
        # records and resource points are filtered by the caller first.
        if sample_time < math.floor(origin) or (ended_at is not None and sample_time > ended_at):
            continue
        if value is None:
            recovered_at = None
            previous = sample_time
            continue
        passes = value >= threshold if higher_is_better else value <= threshold
        if not passes or (
            previous is not None and sample_time - previous > max_gap_seconds
        ):
            recovered_at = None
        if passes and recovered_at is None:
            # k6 buckets may start just before the fractional phase timestamp.
            recovered_at = max(sample_time, origin)
        if passes and sample_time - recovered_at >= window_seconds:
            return recovered_at - origin
        previous = sample_time
    return None


def resource_samples(run_path: Path, field: str) -> list[tuple[float, float]]:
    """Read one process or internal metric from the recovery CSV."""
    return [
        (sample_time, value)
        for sample_time, value in _resource_series(run_path, (field,))[field]
        if value is not None
    ]


def _resource_series(
    run_path: Path, fields: tuple[str, ...], *, preserve_gaps: bool = False
) -> dict[str, list[tuple[float, float | None]]]:
    samples: dict[str, list[tuple[float, float | None]]] = {
        field: [] for field in fields
    }
    path = run_path.parent / "measurement" / "metrics.csv"
    if not path.exists():
        path = path.with_suffix(path.suffix + ".gz")
    if not path.exists():
        return samples
    opener = gzip.open if path.suffix == ".gz" else Path.open
    previous_time: float | None = None
    with opener(path, mode="rt", encoding="utf-8", newline="") as handle:
        for row in csv.DictReader(handle):
            sample_time = timestamp(row.get("timestamp"))
            if sample_time is None:
                if preserve_gaps:
                    # An unplaced sample cannot be excluded from any recovery window.
                    return {field: [] for field in fields}
                continue
            if preserve_gaps and previous_time is not None and sample_time <= previous_time:
                return {field: [] for field in fields}
            previous_time = sample_time
            for field in fields:
                value = finite_float(row.get(field))
                if value is not None or (preserve_gaps and field in row):
                    samples[field].append((sample_time, value))
    return samples


def load_samples(
    run_path: Path, *, started_at: float | None = None, ended_at: float | None = None
) -> dict[str, list[tuple[float, float]]]:
    """Build per-second effective-QPS and P99-latency samples from k6 JSONL."""
    path = run_path.parent / "measurement" / "k6.jsonl"
    if not path.exists():
        path = path.with_suffix(path.suffix + ".gz")
    requests: dict[int, float] = defaultdict(float)
    latencies: dict[int, list[float]] = defaultdict(list)
    if not path.exists():
        return {"effective_qps": [], "latency_p99_ms": []}
    for item in json_lines(path):
        data = item.get("data", {})
        if not isinstance(data, dict):
            continue
        sample_time = timestamp(data.get("time"))
        value = finite_float(data.get("value"))
        if sample_time is None or value is None:
            continue
        if (started_at is not None and sample_time < started_at) or (
            ended_at is not None and sample_time > ended_at
        ):
            continue
        second = int(sample_time)
        if item.get("metric") == "benchmark_requests":
            requests[second] += value
        elif item.get("metric") == "benchmark_latency":
            latencies[second].append(value)
    throughput = []
    previous_second: int | None = None
    for second in sorted(requests):
        if previous_second is not None and second > previous_second + 1:
            # One zero breaks continuity without allocating a potentially huge gap.
            throughput.append((float(previous_second + 1), 0.0))
        throughput.append((max(float(second), started_at or float(second)), requests[second]))
        previous_second = second
    return {
        "effective_qps": throughput,
        "latency_p99_ms": [
            (max(float(key), started_at or float(key)), float(percentile(latencies[key], 0.99)))
            for key in sorted(latencies)
            if latencies[key]
        ],
    }


def summary_of_phase(
    phases: dict[str, tuple[Path, dict[str, Any]]], label: str
) -> dict[str, Any]:
    """Read one phase's summary object from its run artifact.

    A run artifact whose summary is absent or not an object carries no
    evidence: the recovery gates must report it as missing instead of
    raising KeyError/AttributeError on the collector's own output.
    """
    run = phases[label][1]
    summary = run.get("summary") if isinstance(run, dict) else None
    return summary if isinstance(summary, dict) else {}


def recovery_outcome(
    phases: dict[str, tuple[Path, dict[str, Any]]],
    settings: dict[str, Any],
    thresholds: dict[str, float],
) -> dict[str, Any]:
    """Evaluate throughput, latency, process-resource, and quality recovery gates."""
    missing_phases = sorted(set(RECOVERY_PHASES) - set(phases))
    if missing_phases:
        return {
            "verdict": "INCONCLUSIVE",
            "missing": [f"phase:{name}" for name in missing_phases],
            "failed": [],
            "seconds": {},
        }
    stable = summary_of_phase(phases, "stable")
    recover_path, recover_run = phases["recover"]
    started_at = numeric(recover_run.get("started_at_unix"))
    if started_at is None:
        return {
            "verdict": "INCONCLUSIVE",
            "missing": ["recover.started_at_unix"],
            "failed": [],
            "seconds": {},
        }
    ended_at = numeric(recover_run.get("ended_at_unix"))
    if "ended_at_unix" in recover_run and (ended_at is None or ended_at <= started_at):
        return {
            "verdict": "INCONCLUSIVE",
            "missing": ["recover.ended_at_unix"],
            "failed": [],
            "seconds": {},
        }
    recover = summary_of_phase(phases, "recover")
    tolerance = settings["tolerance_ratio"]
    window = settings["recovery_window_seconds"]
    load = load_samples(recover_path, started_at=started_at, ended_at=ended_at)
    resources = _resource_series(
        recover_path,
        ("rss_mb", "channel_length", "connection_cache_bytes"),
        preserve_gaps=True,
    )
    resources = {
        field: [
            (sample_time, value)
            for sample_time, value in samples
            if sample_time >= started_at and (ended_at is None or sample_time <= ended_at)
        ]
        for field, samples in resources.items()
    }
    specifications = {
        "effective_qps": (
            nested(stable, "effective_qps"),
            load["effective_qps"],
            True,
        ),
        "latency_p99_ms": (
            nested(stable, "latency_ms", "p99"),
            load["latency_p99_ms"],
            False,
        ),
        "rss_mb": (
            nested(stable, "resources", "rss_mb", "avg"),
            resources["rss_mb"],
            False,
        ),
    }
    optional_specifications = {
        "channel_length": (
            nested(stable, "resources", "channel_length", "avg"),
            resources["channel_length"],
            False,
        ),
        "connection_cache_bytes": (
            nested(stable, "resources", "connection_cache_bytes", "avg"),
            resources["connection_cache_bytes"],
            False,
        ),
    }
    specifications.update(
        {
            name: specification
            for name, specification in optional_specifications.items()
            if specification[0] is not None and specification[1]
        }
    )
    seconds = {
        name: continuous_recovery(
            samples,
            # A reference that is not a finite number is unusable evidence,
            # not a threshold: a boolean is an int subclass (True would gate
            # at 1.0) and a non-finite latency reference with
            # lower-is-better passes every sample, so both must read as
            # missing instead of silently gating the recovery.
            finite_float(reference),
            tolerance,
            window,
            higher_is_better=higher_is_better,
            started_at=started_at,
            ended_at=ended_at,
        )
        for name, (reference, samples, higher_is_better) in specifications.items()
    }
    missing = [name for name, value in seconds.items() if value is None]
    failed = [
        name
        for name, value in seconds.items()
        if value is not None and value > thresholds["max_recovery_seconds"]
    ]
    quality_checks = {
        "trace_completeness": (
            bounded_measurement(nested(recover, "trace_completeness"), maximum=1),
            thresholds["min_trace_completeness"],
        ),
    }
    for label in RECOVERY_PHASES:
        phase_run = phases[label][1]
        phase_summary = phase_run.get("summary", {})
        quality_checks[f"{label}_process_survived"] = (
            nested(phase_summary, "process_survived"),
            True,
        )
        quality_checks[f"{label}_runtime_clean"] = (
            nested(phase_summary, "runtime_clean"),
            True,
        )
    for label in ("stable", "recover"):
        verdict = phases[label][1].get("evaluation", {}).get("verdict")
        quality_checks[f"{label}_load_gates"] = (verdict == "PASS", True)
    for name, (actual, expected) in quality_checks.items():
        if actual is None:
            missing.append(name)
        elif not meets(actual, expected):
            failed.append(name)
    verdict = "INCONCLUSIVE" if missing else "FAIL" if failed else "PASS"
    return {
        "verdict": verdict,
        "missing": missing,
        "failed": failed,
        "seconds": seconds,
    }


def fault_outcome(
    run_path: Path,
    run: dict[str, Any],
    settings: dict[str, Any],
    thresholds: dict[str, float],
) -> dict[str, Any]:
    """Evaluate malformed-input counts, liveness, and normal-traffic quality."""
    path = run_path.parent / "measurement" / "fault-results.json"
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        # Interrupted writes are missing evidence, just like an absent file.
        value = None
    if not isinstance(value, dict):
        return {
            "verdict": "INCONCLUSIVE",
            "missing": ["fault-results.json"],
            "failed": [],
        }
    outcomes = value.get("outcomes", {})
    if not isinstance(outcomes, dict):
        outcomes = {}
    usable: dict[str, int] = {}
    malformed: set[str] = set()
    for name in FAULT_CASES:
        entry = outcomes.get(name)
        if isinstance(entry, dict) and all(
            isinstance(count, int)
            and not isinstance(count, bool)
            and count >= 0
            for count in entry.values()
        ):
            usable[name] = sum(entry.values())
        else:
            malformed.add(name)
    missing = [f"case:{name}" for name in sorted(malformed)]
    failed = [
        f"count:{name}"
        for name in sorted(usable)
        if usable[name] != settings["repetitions_per_case"]
    ]
    for name in ("server_healthy_after", "process_alive_before", "process_alive_after"):
        if value.get(name) is not True:
            failed.append(name)
    failed.extend(
        f"delivery:{name}" for name in delivery_failures({name: outcomes[name] for name in usable})
    )
    if "sse_truncation_verified" not in value:
        missing.append("sse_truncation_verified")
    elif value["sse_truncation_verified"] is not True:
        failed.append("sse_truncation_verified")
    summary = run.get("summary", {})
    checks = {
        "http_success_rate": (
            bounded_measurement(nested(summary, "http_success_rate"), maximum=1),
            thresholds["min_http_success_rate"],
        ),
        "trace_completeness": (
            bounded_measurement(nested(summary, "trace_completeness"), maximum=1),
            thresholds["min_trace_completeness"],
        ),
        "token_accuracy": (
            bounded_measurement(nested(summary, "token_accuracy"), maximum=1),
            thresholds["min_token_accuracy"],
        ),
        "process_survived": (nested(summary, "process_survived"), True),
        "runtime_clean": (nested(summary, "runtime_clean"), True),
    }
    for name, (actual, expected) in checks.items():
        if actual is None:
            missing.append(name)
        elif not meets(actual, expected):
            failed.append(name)
    verdict = "INCONCLUSIVE" if missing else "FAIL" if failed else "PASS"
    return {"verdict": verdict, "missing": missing, "failed": failed}


def confirmation_verdicts(evidence: object, level: int) -> list[str]:
    """Verdict strings a capacity result recorded for one QPS level.

    The confirmation map comes from the capacity probe's own result file:
    a non-object map, a non-list entry, or non-string verdicts are
    incomplete evidence and read as no verdicts. Reading them as `str`
    would silently pass `"PASSPASSPASS"` as three passes through
    ``str.count``'s substring semantics.
    """
    entries = evidence.get(str(level)) if isinstance(evidence, dict) else None
    if not isinstance(entries, list):
        return []
    return [entry for entry in entries if isinstance(entry, str)]


def audit_campaign(
    campaign_data: dict[str, Any],
    items: list[tuple[Path, dict[str, Any]]],
    capacities: dict[str, dict[str, Any]],
    recovery: dict[tuple[str, int], dict[str, Any]],
    faults: dict[str, dict[str, Any]],
    regression: dict[str, Any],
) -> list[str]:
    """Return every reason the formal campaign cannot yet declare PASS."""
    issues = []
    resolution = campaign_data["capacity"]["qps_resolution"]
    confirmations = campaign_data["capacity"]["confirm_repetitions"]
    # A confirmation needs a majority of its repetitions; mirror campaign.py's
    # rule so confirm_repetitions=1 does not demand two verdicts.
    required_confirmations = confirmations // 2 + 1
    for version in VERSIONS:
        value = capacities.get(version, {})
        maximum = value.get("maximum_sustainable_qps")
        failure = value.get("first_failed_qps")
        if value.get("safety_limit_reached") or not value.get("boundary_confirmed"):
            issues.append(f"{version} capacity boundary is not confirmed")
            continue
        if not isinstance(maximum, int) or failure != maximum + resolution:
            issues.append(f"{version} capacity lacks an adjacent failed QPS")
            continue
        # The confirmation map comes from the capacity probe's own result
        # file, so an unusable shape is incomplete evidence — the audit's
        # contract is to enumerate every blocking reason, never to raise.
        # A non-list entry must also not fall through to str.count, whose
        # substring semantics would count "PASSPASSPASS" as three passes.
        evidence = value.get("confirmation", {})
        passes = confirmation_verdicts(evidence, maximum)
        fails = confirmation_verdicts(evidence, failure)
        if passes.count("PASS") < required_confirmations:
            issues.append(f"{version} capacity pass confirmation is incomplete")
        if fails.count("FAIL") < required_confirmations:
            issues.append(f"{version} capacity fail confirmation is incomplete")
        if len(passes) < confirmations or len(fails) < confirmations:
            issues.append(f"{version} capacity repetitions are incomplete")

    maxima = [
        capacities.get(version, {}).get("maximum_sustainable_qps")
        for version in VERSIONS
    ]
    if all(isinstance(value, int) and value > 0 for value in maxima):
        common = min(maxima)
        matrix_qps = campaign_data["matrix"].get("qps") or [
            max(resolution, int(common * fraction / resolution) * resolution)
            for fraction in (0.2, 0.4, 0.6, 0.8, 1.0)
        ]
        soak_qps = int(common * 0.8 / resolution) * resolution
        lower_versions = [
            capacities[version]
            for version in VERSIONS
            if capacities[version].get("maximum_sustainable_qps") == common
        ]
        failed_qps = [value.get("first_failed_qps") for value in lower_versions]
        overload_qps = (
            min(failed_qps)
            if failed_qps and all(isinstance(value, int) for value in failed_qps)
            else None
        )
    else:
        matrix_qps = []
        soak_qps = None
        overload_qps = None
    if len(matrix_qps) != 5 or len(set(matrix_qps)) != 5:
        issues.append("five unique common matrix QPS levels are unavailable")
    grouped: dict[tuple[str, str, str, int], list[dict[str, Any]]] = defaultdict(list)
    for _, run in items:
        repetition = run.get("repetition")
        grouped[
            (
                str(run.get("scenario")),
                str(run.get("version")),
                str(run.get("label")),
                int(repetition) if isinstance(repetition, (int, float)) else 0,
            )
        ].append(run)
    matrix = campaign_data["matrix"]
    for version in VERSIONS:
        for qps in matrix_qps:
            for repetition in range(1, matrix["repetitions"] + 1):
                runs = grouped[("matrix", version, f"qps-{qps}", repetition)]
                if len(runs) != 1:
                    issues.append(
                        f"{version} matrix {qps} QPS rep {repetition} is missing"
                    )
                elif (
                    runs[0].get("qps") != qps
                    or runs[0].get("duration_seconds") != matrix["duration_seconds"]
                    or runs[0].get("warmup_seconds") != matrix["warmup_seconds"]
                    or runs[0].get("evaluation", {}).get("verdict") != "PASS"
                ):
                    issues.append(
                        f"{version} matrix {qps} QPS rep {repetition} did not pass"
                    )

        soak = [
            run
            for _, run in items
            if run.get("scenario") == "soak" and run.get("version") == version
        ]
        soak_settings = campaign_data["soak"]
        if len(soak) != 1:
            issues.append(f"{version} soak run is missing")
        else:
            if (
                soak[0].get("qps") != soak_qps
                or soak[0].get("label") != f"qps-{soak_qps}"
                or soak[0].get("duration_seconds") != soak_settings["duration_seconds"]
                or soak[0].get("warmup_seconds") != soak_settings["warmup_seconds"]
                or soak[0].get("evaluation", {}).get("verdict") != "PASS"
            ):
                issues.append(f"{version} soak run did not pass the frozen window")

        for repetition in range(1, campaign_data["recovery"]["repetitions"] + 1):
            durations = {
                "stable": campaign_data["recovery"]["stable_seconds"],
                "overload": campaign_data["recovery"]["overload_seconds"],
                "recover": campaign_data["recovery"]["recover_seconds"],
            }
            for label, duration in durations.items():
                runs = grouped[("recovery", version, label, repetition)]
                expected_qps = overload_qps if label == "overload" else soak_qps
                if len(runs) != 1:
                    issues.append(
                        f"{version} recovery {label} rep {repetition} is missing"
                    )
                elif (
                    runs[0].get("qps") != expected_qps
                    or runs[0].get("duration_seconds") != duration
                    or runs[0].get("warmup_seconds") != 0
                ):
                    issues.append(
                        f"{version} recovery {label} rep {repetition} has drifted inputs"
                    )
            if recovery.get((version, repetition), {}).get("verdict") != "PASS":
                issues.append(f"{version} recovery rep {repetition} did not pass")
        fault_runs = [
            run
            for _, run in items
            if run.get("scenario") == "fault" and run.get("version") == version
        ]
        fault_settings = campaign_data["fault"]
        if len(fault_runs) != 1 or (
            fault_runs
            and (
                fault_runs[0].get("qps") != soak_qps
                or fault_runs[0].get("duration_seconds")
                != fault_settings["duration_seconds"]
                or fault_runs[0].get("warmup_seconds")
                != fault_settings["warmup_seconds"]
            )
        ):
            issues.append(f"{version} fault run is missing or has drifted inputs")
        if faults.get(version, {}).get("verdict") != "PASS":
            issues.append(f"{version} fault run did not pass")

    checks = regression.get("checks", [])
    checks = checks if isinstance(checks, list) else []
    usable_checks = [check for check in checks if usable_regression_check(check)]
    tools = {regression_tool(check) for check in usable_checks}
    if (
        not checks
        or len(usable_checks) != len(checks)
        or any(check["exit_code"] != 0 for check in usable_checks)
    ):
        issues.append("regression checks are missing or failed")
    if regression.get("full") is not True:
        issues.append("full Rust regression gates were not recorded")
    for required in REQUIRED_REGRESSION_TOOLS:
        if required not in tools:
            issues.append(f"regression evidence is missing {required}")
    return issues
