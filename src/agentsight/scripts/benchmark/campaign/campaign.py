#!/usr/bin/env python3
"""Run the frozen AgentSight capacity, matrix, soak, recovery, and fault campaign."""

from __future__ import annotations

import argparse
import json
import math
import subprocess
import time
from collections.abc import Callable
from contextlib import AbstractContextManager, nullcontext
from pathlib import Path
from typing import Any

from campaign_config import safety_settings, validate_campaign
from campaign_evaluation import evaluate
from campaign_manifest import ensure_manifest
from campaign_runtime import capture_runtime_log, log_position, process_metadata

SCRIPT_DIR = Path(__file__).resolve().parent
CAPACITY_DEFERRED_GATES = {"rss_slope_mb_per_hour"}
SAFETY_EXIT_CODE = 75


def align_down(value: float, resolution: int) -> int:
    """Round a positive QPS value down to the configured search grid."""
    return max(resolution, math.floor(value / resolution) * resolution)


def align_up(value: float, resolution: int) -> int:
    """Round a positive QPS value up to the configured search grid."""
    return max(resolution, math.ceil(value / resolution) * resolution)


def capacity_verdict(result: dict[str, Any]) -> str:
    """Return a monotonic capacity verdict, deferring long-term trend gates."""
    evaluation = result["evaluation"]
    verdict = evaluation["verdict"]
    if not any(key in evaluation for key in ("missing", "failed")):
        return verdict
    missing = set(evaluation.get("missing", ())) - CAPACITY_DEFERRED_GATES
    failed = set(evaluation.get("failed", ())) - CAPACITY_DEFERRED_GATES
    if missing:
        return "INCONCLUSIVE"
    return "FAIL" if failed else "PASS"


def read_json(path: Path) -> dict[str, Any]:
    """Read a JSON object and reject other top-level values."""
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise TypeError(f"expected a JSON object: {path}")
    return value


def run_once(
    campaign: dict[str, Any],
    results_root: Path,
    version_name: str,
    scenario: str,
    label: str,
    qps: int,
    duration: int,
    warmup: int,
    repetition: int,
    pid_override: int | None,
    faults: int = 0,
    resume: bool = False,
) -> dict[str, Any]:
    """Run one isolated warmup plus formal measurement and record its verdict."""
    version = campaign["versions"][version_name]
    pid = pid_override or version.get("pid")
    if not pid or not Path(f"/proc/{pid}").exists():
        raise ValueError(f"{version_name} AgentSight pid is missing or not alive")
    output = (
        results_root / "runs" / scenario / version_name / label / f"rep-{repetition}"
    )
    if output.exists():
        result_path = output / "run-result.json"
        if resume and result_path.is_file():
            print(f"[SKIP] completed run: {result_path}", flush=True)
            return read_json(result_path)
        if not resume:
            raise FileExistsError(
                f"run output already exists; use --resume or a new campaign ID: {output}"
            )
        archive = (
            results_root
            / "incomplete"
            / scenario
            / version_name
            / label
            / f"rep-{repetition}-{time.time_ns()}"
        )
        archive.parent.mkdir(parents=True, exist_ok=True)
        output.rename(archive)
        print(f"[ARCHIVE] incomplete run: {archive}", flush=True)
    output.mkdir(parents=True, exist_ok=True)
    common = [
        str(SCRIPT_DIR.parent / "single_run" / "run.sh"),
        "--protocol",
        campaign["load"]["protocol"],
        "--qps",
        str(qps),
        "--payload-kb",
        str(campaign["load"]["payload_kb"]),
        "--chunks",
        str(campaign["load"]["chunks"]),
        "--chunk-bytes",
        str(campaign["load"]["chunk_bytes"]),
        "--agentsight-pid",
        str(pid),
        "--external-server",
        "--request-log-dir",
        str(results_root / "runtime" / "mock-server" / "request-logs"),
    ]
    safety = safety_settings(campaign)
    common.extend(
        [
            "--results-root",
            str(results_root),
            "--max-results-gb",
            str(safety["max_results_gb"]),
            "--min-free-disk-gb",
            str(safety["min_free_disk_gb"]),
            "--min-available-memory-mb",
            str(safety["min_available_memory_mb"]),
            "--max-agentsight-rss-mb",
            str(safety["max_agentsight_rss_mb"]),
            "--max-k6-vus",
            str(safety["max_k6_vus"]),
        ]
    )
    measurement_args = list(common)
    if version.get("db"):
        measurement_args.extend(["--db", version["db"]])
    if version.get("metrics_file"):
        measurement_args.extend(["--metrics-file", version["metrics_file"]])
    if warmup > 0:
        warmup_output = output / "warmup"
        warmup_command = common + [
            "--duration",
            str(warmup),
            "--output-dir",
            str(warmup_output),
        ]
        with (output / "warmup.log").open("w", encoding="utf-8") as log:
            warmup_status = subprocess.run(
                warmup_command, stdout=log, stderr=subprocess.STDOUT, check=False
            ).returncode
        if warmup_status != 0:
            safety_stop_path = warmup_output / "safety-stop.json"
            if warmup_status == SAFETY_EXIT_CODE or safety_stop_path.exists():
                safety_stop = (
                    read_json(safety_stop_path) if safety_stop_path.exists() else {}
                )
                reason = safety_stop.get("reason", "unknown")
                raise RuntimeError(
                    f"benchmark safety guard stopped {version_name}:{scenario} "
                    f"during warmup ({reason}); inspect {safety_stop_path}"
                )
            if not (warmup_output / "run-summary.json").is_file():
                raise RuntimeError(f"warmup failed; inspect {output / 'warmup.log'}")
            print(
                f"[WARMUP] {version_name}:{scenario} returned status "
                f"{warmup_status}; continuing to the measured probe",
                flush=True,
            )
    runtime_log = Path(version["log_file"]) if version.get("log_file") else None
    runtime_log_start = log_position(runtime_log)
    command = measurement_args + [
        "--duration",
        str(duration),
        "--output-dir",
        str(output / "measurement"),
    ]
    if faults:
        command.extend(["--fault-count", str(faults)])
    started = time.time()
    with (output / "harness.log").open("w", encoding="utf-8") as log:
        status = subprocess.run(
            command, stdout=log, stderr=subprocess.STDOUT, check=False
        ).returncode
    summary_path = output / "measurement" / "run-summary.json"
    summary = read_json(summary_path) if summary_path.exists() else {}
    safety_stop_path = output / "measurement" / "safety-stop.json"
    safety_stop = read_json(safety_stop_path) if safety_stop_path.exists() else None
    if safety_stop is not None:
        summary["safety_stop"] = safety_stop
    runtime_clean, runtime_errors = capture_runtime_log(
        runtime_log, runtime_log_start, output / "measurement" / "agentsight.log"
    )
    summary["runtime_clean"] = runtime_clean
    summary["runtime_errors"] = runtime_errors
    if summary_path.exists():
        summary_path.write_text(
            json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
    evaluation = evaluate(summary, campaign["thresholds"], scenario)
    if status != 0:
        evaluation["verdict"] = "FAIL"
        if "harness_exit_code" not in evaluation["failed"]:
            evaluation["failed"].append("harness_exit_code")
    result = {
        "schema_version": 1,
        "scenario": scenario,
        "label": label,
        "version": version_name,
        "metrics_enabled": bool(version.get("metrics_file")),
        "qps": qps,
        "duration_seconds": duration,
        "warmup_seconds": warmup,
        "repetition": repetition,
        "started_at_unix": started,
        "ended_at_unix": time.time(),
        "harness_exit_code": status,
        "process": process_metadata(pid),
        "summary": summary,
        "evaluation": evaluation,
    }
    (output / "run-result.json").write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    if status == SAFETY_EXIT_CODE:
        reason = safety_stop.get("reason") if safety_stop else "unknown"
        raise RuntimeError(
            f"benchmark safety guard stopped {version_name}:{scenario} "
            f"({reason}); inspect {safety_stop_path}"
        )
    return result


def capacity(
    campaign: dict[str, Any],
    results: Path,
    version: str,
    pid: int | None,
    resume: bool = False,
    process_factory: Callable[[], AbstractContextManager[int]] | None = None,
) -> dict[str, Any]:
    """Estimate, locate by binary search, and confirm the sustainable-QPS boundary."""
    results.mkdir(parents=True, exist_ok=True)
    settings = campaign["capacity"]
    resolution = settings["qps_resolution"]
    safety_max = align_down(settings["qps_safety_max"], resolution)

    pretest_probes: dict[str, str] = {}
    pretest_low = 0
    pretest_fail: int | None = None

    def run_probe(
        label: str,
        qps: int,
        duration: int,
        warmup: int,
        repetition: int,
    ) -> dict[str, Any]:
        process = process_factory() if process_factory else nullcontext(pid)
        with process as active_pid:
            return run_once(
                campaign,
                results,
                version,
                "capacity",
                label,
                qps,
                duration,
                warmup,
                repetition,
                active_pid,
                resume=resume,
            )

    def probe_pretest(qps: int) -> str:
        print(f"[CAPACITY] {version} pretest qps={qps}", flush=True)
        result = run_probe(
            f"pretest-{qps}",
            qps,
            settings["pretest_duration_seconds"],
            settings["pretest_warmup_seconds"],
            1,
        )
        verdict = capacity_verdict(result)
        pretest_probes[str(qps)] = verdict
        if verdict == "INCONCLUSIVE":
            raise RuntimeError("capacity pretest needs every frozen threshold metric")
        print(f"[CAPACITY] {version} pretest qps={qps}: {verdict}", flush=True)
        return verdict

    qps = min(settings["qps_start"], safety_max)
    if probe_pretest(qps) == "PASS":
        pretest_low = qps
        while qps < safety_max:
            next_qps = min(safety_max, qps * 2)
            verdict = probe_pretest(next_qps)
            if verdict == "FAIL":
                pretest_fail = next_qps
                break
            pretest_low = next_qps
            qps = next_qps
    else:
        pretest_fail = qps
        while qps > resolution:
            next_qps = align_down(qps / 2, resolution)
            if next_qps >= qps:
                break
            verdict = probe_pretest(next_qps)
            if verdict == "PASS":
                pretest_low = next_qps
                break
            pretest_fail = next_qps
            qps = next_qps

    if pretest_low and pretest_fail:
        estimate = (pretest_low + pretest_fail) // 2
    else:
        estimate = pretest_low or pretest_fail or resolution
    search_start = min(
        safety_max,
        align_down(estimate * settings["search_start_ratio"], resolution),
    )

    low = 0
    first_fail: int | None = None
    probes: dict[str, str] = {}

    def probe_search(qps: int) -> str:
        print(f"[CAPACITY] {version} formal search qps={qps}", flush=True)
        result = run_probe(
            f"search-{qps}",
            qps,
            settings["probe_duration_seconds"],
            settings["probe_warmup_seconds"],
            1,
        )
        verdict = capacity_verdict(result)
        probes[str(qps)] = verdict
        if verdict == "INCONCLUSIVE":
            raise RuntimeError("capacity search needs every frozen threshold metric")
        print(f"[CAPACITY] {version} formal search qps={qps}: {verdict}", flush=True)
        return verdict

    if probe_search(search_start) == "PASS":
        low = search_start
    else:
        first_fail = search_start
        candidate = (
            pretest_low
            if 0 < pretest_low < first_fail
            else align_down(search_start / 2, resolution)
        )
        while candidate < first_fail:
            verdict = probe_search(candidate)
            if verdict == "PASS":
                low = candidate
                break
            first_fail = candidate
            if candidate == resolution:
                break
            candidate = align_down(candidate / 2, resolution)

    search_initial_upper: int | None = None
    if low and first_fail is None:
        candidate = (
            align_up(pretest_fail, resolution)
            if pretest_fail is not None
            else safety_max
        )
        candidate = min(safety_max, max(low + resolution, candidate))
        search_initial_upper = candidate if candidate > low else None
        while candidate > low:
            verdict = probe_search(candidate)
            if verdict == "FAIL":
                first_fail = candidate
                break
            low = candidate
            if low >= safety_max:
                break
            candidate = min(safety_max, align_up(low * 2, resolution))

    while low and first_fail and first_fail - low > resolution:
        steps = (first_fail - low) // resolution
        candidate = low + (steps // 2) * resolution
        verdict = probe_search(candidate)
        if verdict == "PASS":
            low = candidate
        else:
            first_fail = candidate

    confirmation: dict[str, list[str]] = {}

    def confirm(value: int) -> list[str]:
        if str(value) in confirmation:
            return confirmation[str(value)]
        verdicts = []
        for repetition in range(1, settings["confirm_repetitions"] + 1):
            print(
                f"[CAPACITY] {version} confirm qps={value} " f"repetition={repetition}",
                flush=True,
            )
            result = run_probe(
                f"confirm-{value}",
                value,
                settings["confirm_duration_seconds"],
                settings["confirm_warmup_seconds"],
                repetition,
            )
            verdict = capacity_verdict(result)
            verdicts.append(verdict)
            print(
                f"[CAPACITY] {version} confirm qps={value} "
                f"repetition={repetition}: {verdict}",
                flush=True,
            )
        confirmation[str(value)] = verdicts
        return verdicts

    if not low:
        report = {
            "version": version,
            "search_method": "automatic-pretest-80pct-binary",
            "search_start_ratio": settings["search_start_ratio"],
            "pretest": {
                "estimate_qps": estimate,
                "last_passed_qps": None,
                "first_failed_qps": pretest_fail,
                "deferred_gates": sorted(CAPACITY_DEFERRED_GATES),
                "probes": pretest_probes,
            },
            "deferred_gates": sorted(CAPACITY_DEFERRED_GATES),
            "search_start_qps": search_start,
            "search_initial_upper_qps": search_initial_upper,
            "maximum_sustainable_qps": None,
            "confirmed_lower_bound_qps": None,
            "first_failed_qps": None,
            "located_first_failed_qps": first_fail,
            "safety_limit_reached": False,
            "boundary_confirmed": False,
            "probes": probes,
            "confirmation": confirmation,
        }
        report_path = results / f"capacity-{version}.json"
        report_path.write_text(
            json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        raise RuntimeError(
            f"{version} failed at the minimum capacity search level "
            f"({resolution} QPS); lower capacity.qps_resolution and start a new "
            f"campaign; inspect {report_path}"
        )

    required_confirmations = settings["confirm_repetitions"] // 2 + 1

    def confirmation_outcome(value: int) -> str:
        verdicts = confirm(value)
        if verdicts.count("PASS") >= required_confirmations:
            return "PASS"
        if verdicts.count("FAIL") >= required_confirmations:
            return "FAIL"
        raise RuntimeError(
            f"capacity confirmation is inconclusive at {value} QPS: {verdicts}"
        )

    confirmed_pass: int | None = None
    confirmed_fail: int | None = None
    initial_outcome = confirmation_outcome(low)
    if initial_outcome == "PASS":
        confirmed_pass = low
        upper_candidate = first_fail
        while upper_candidate is not None:
            if confirmation_outcome(upper_candidate) == "FAIL":
                confirmed_fail = upper_candidate
                break
            confirmed_pass = upper_candidate
            if upper_candidate >= safety_max:
                break
            known_failures = [
                int(value)
                for value, verdict in probes.items()
                if verdict == "FAIL"
                and int(value) > upper_candidate
                and value not in confirmation
            ]
            upper_candidate = (
                min(known_failures)
                if known_failures
                else min(
                    safety_max,
                    max(upper_candidate + resolution, upper_candidate * 2),
                )
            )
    else:
        confirmed_fail = low
        while confirmed_fail > resolution:
            known_passes = [
                int(value)
                for value, verdict in probes.items()
                if verdict == "PASS"
                and int(value) < confirmed_fail
                and value not in confirmation
            ]
            lower_candidate = (
                max(known_passes)
                if known_passes
                else align_down(confirmed_fail / 2, resolution)
            )
            if lower_candidate >= confirmed_fail:
                lower_candidate = confirmed_fail - resolution
            if confirmation_outcome(lower_candidate) == "PASS":
                confirmed_pass = lower_candidate
                break
            confirmed_fail = lower_candidate

    while (
        confirmed_pass is not None
        and confirmed_fail is not None
        and confirmed_fail - confirmed_pass > resolution
    ):
        steps = (confirmed_fail - confirmed_pass) // resolution
        candidate = confirmed_pass + (steps // 2) * resolution
        if confirmation_outcome(candidate) == "PASS":
            confirmed_pass = candidate
        else:
            confirmed_fail = candidate

    confirmed_lower_bound = max(
        (
            int(value)
            for value, verdicts in confirmation.items()
            if verdicts.count("PASS") >= required_confirmations
        ),
        default=None,
    )
    confirmed_failures = [
        int(value)
        for value, verdicts in confirmation.items()
        if verdicts.count("FAIL") >= required_confirmations
        and (confirmed_lower_bound is None or int(value) > confirmed_lower_bound)
    ]
    adjacent_failure = min(confirmed_failures, default=None)
    boundary_confirmed = (
        confirmed_lower_bound is not None
        and adjacent_failure == confirmed_lower_bound + settings["qps_resolution"]
    )
    maximum = confirmed_lower_bound if boundary_confirmed else None
    report = {
        "version": version,
        "search_method": "automatic-pretest-80pct-binary",
        "confirmation_search_method": "adaptive-binary",
        "search_start_ratio": settings["search_start_ratio"],
        "pretest": {
            "estimate_qps": estimate,
            "last_passed_qps": pretest_low or None,
            "first_failed_qps": pretest_fail,
            "deferred_gates": sorted(CAPACITY_DEFERRED_GATES),
            "probes": pretest_probes,
        },
        "deferred_gates": sorted(CAPACITY_DEFERRED_GATES),
        "search_start_qps": search_start,
        "search_initial_upper_qps": search_initial_upper,
        "maximum_sustainable_qps": maximum,
        "confirmed_lower_bound_qps": confirmed_lower_bound,
        "first_failed_qps": adjacent_failure,
        "located_first_failed_qps": first_fail,
        "safety_limit_reached": first_fail is None,
        "boundary_confirmed": boundary_confirmed,
        "probes": probes,
        "confirmation": confirmation,
    }
    (results / f"capacity-{version}.json").write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return report


def resolved_qps(campaign: dict[str, Any], results: Path) -> tuple[list[int], int, int]:
    """Resolve the common matrix, soak, and overload QPS values."""
    capacities = [
        read_json(results / f"capacity-{name}.json") for name in campaign["versions"]
    ]
    maxima = [item.get("maximum_sustainable_qps") for item in capacities]
    if not all(isinstance(value, int) and value > 0 for value in maxima):
        raise ValueError("both versions need confirmed capacity results")
    common = min(maxima)
    resolution = campaign["capacity"]["qps_resolution"]
    matrix = campaign["matrix"].get("qps") or [
        max(resolution, int(common * fraction / resolution) * resolution)
        for fraction in (0.2, 0.4, 0.6, 0.8, 1.0)
    ]
    if len(set(matrix)) != 5:
        raise ValueError("common capacity is too low for five unique matrix QPS values")
    soak = int(common * 0.8 / resolution) * resolution
    lower_capacity_versions = [
        item for item in capacities if item.get("maximum_sustainable_qps") == common
    ]
    failed_values = [item.get("first_failed_qps") for item in lower_capacity_versions]
    if not all(isinstance(value, int) and value > common for value in failed_values):
        raise ValueError("lower-capacity version needs an adjacent failed QPS")
    overload = min(failed_values)
    resolution_path = results / "campaign-resolution.json"
    resolved = {
        "schema_version": 1,
        "common_max_qps": common,
        "matrix_qps": matrix,
        "soak_qps": soak,
        "overload_qps": overload,
    }
    if resolution_path.exists() and read_json(resolution_path) != resolved:
        raise ValueError("resolved campaign load levels changed after first use")
    resolution_path.write_text(
        json.dumps(resolved, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return matrix, soak, overload


def run_scenario(
    campaign: dict[str, Any],
    results: Path,
    version: str,
    scenario: str,
    pid: int | None,
    resume: bool = False,
) -> None:
    """Run one non-capacity scenario for a selected version."""
    if scenario == "smoke":
        settings = campaign["smoke"]
        run_once(
            campaign,
            results,
            version,
            scenario,
            f"qps-{settings['qps']}",
            settings["qps"],
            settings["duration_seconds"],
            0,
            1,
            pid,
            resume=resume,
        )
        return
    matrix, soak_qps, overload_qps = resolved_qps(campaign, results)
    if scenario == "matrix":
        settings = campaign["matrix"]
        for qps in matrix:
            for repetition in range(1, settings["repetitions"] + 1):
                run_once(
                    campaign,
                    results,
                    version,
                    scenario,
                    f"qps-{qps}",
                    qps,
                    settings["duration_seconds"],
                    settings["warmup_seconds"],
                    repetition,
                    pid,
                    resume=resume,
                )
    elif scenario == "soak":
        settings = campaign["soak"]
        run_once(
            campaign,
            results,
            version,
            scenario,
            f"qps-{soak_qps}",
            soak_qps,
            settings["duration_seconds"],
            settings["warmup_seconds"],
            1,
            pid,
            resume=resume,
        )
    elif scenario == "recovery":
        settings = campaign["recovery"]
        phases = (
            ("stable", soak_qps, settings["stable_seconds"]),
            ("overload", overload_qps, settings["overload_seconds"]),
            ("recover", soak_qps, settings["recover_seconds"]),
        )
        for repetition in range(1, settings["repetitions"] + 1):
            for label, qps, duration in phases:
                run_once(
                    campaign,
                    results,
                    version,
                    scenario,
                    label,
                    qps,
                    duration,
                    0,
                    repetition,
                    pid,
                    resume=resume,
                )
    elif scenario == "fault":
        settings = campaign["fault"]
        run_once(
            campaign,
            results,
            version,
            scenario,
            f"qps-{soak_qps}",
            soak_qps,
            settings["duration_seconds"],
            settings["warmup_seconds"],
            1,
            pid,
            faults=settings["repetitions_per_case"],
            resume=resume,
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--campaign", type=Path, required=True)
    parser.add_argument("--results", type=Path, required=True)
    parser.add_argument("--version", choices=("baseline", "optimized"))
    parser.add_argument(
        "--scenario",
        choices=(
            "validate",
            "smoke",
            "capacity",
            "matrix",
            "soak",
            "recovery",
            "fault",
        ),
        default="validate",
    )
    parser.add_argument("--pid", type=int, help="override the selected version pid")
    parser.add_argument(
        "--resume",
        action="store_true",
        help="reuse completed runs and archive incomplete attempts",
    )
    args = parser.parse_args()
    campaign_data = read_json(args.campaign)
    validate_campaign(campaign_data)
    ensure_manifest(args.results, args.campaign, campaign_data)
    if args.scenario == "validate":
        print(f"campaign frozen: {args.results / 'manifest.json'}")
        return 0
    if not args.version:
        parser.error("--version is required when running a scenario")
    if args.scenario == "capacity":
        capacity(campaign_data, args.results, args.version, args.pid, args.resume)
    else:
        run_scenario(
            campaign_data,
            args.results,
            args.version,
            args.scenario,
            args.pid,
            args.resume,
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
