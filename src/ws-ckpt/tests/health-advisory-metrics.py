#!/usr/bin/env python3
"""Run and summarize concurrent health-advisory CLI workload samples."""

from __future__ import annotations

import argparse
import csv
import json
import math
import os
import re
import shlex
import signal
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Sequence

OPERATION_NAMES = ("checkpoint", "rollback", "delete")
OUTCOME_NAMES = ("success", "failure", "unknown")
OPERATION_HEADER = (
    "iteration",
    "operation",
    "start_ns",
    "duration_ns",
    "exit_code",
    "status",
)
LATENCY_HEADER = ("operation", "outcome", "count", "p50_ns", "p95_ns", "p99_ns", "max_ns")
SUMMARY_HEADER = ("metric", "value")
RUN_ID_PATTERN = re.compile(r"^[A-Za-z0-9-]{1,24}$")
_SIGNAL_GENERATION = 0
_LAST_SIGNAL = 0
_CURRENT_CHILD: subprocess.Popen[str] | None = None
_CANCEL_KILL_ARMED = False


@dataclass(frozen=True)
class CliResult:
    """One CLI attempt and its measured process lifetime."""

    stdout: str
    exit_code: int
    status: str
    start_ns: int
    duration_ns: int
    spawned: bool


def _signal_handler(signum: int, _frame: Any) -> None:
    """Record cancellation and ask the active CLI child to stop."""
    global _CANCEL_KILL_ARMED, _SIGNAL_GENERATION, _LAST_SIGNAL
    _SIGNAL_GENERATION += 1
    _LAST_SIGNAL = signum
    child = _CURRENT_CHILD
    if child is not None and child.poll() is None:
        if not _CANCEL_KILL_ARMED:
            signal.setitimer(signal.ITIMER_REAL, 3.0)
            _CANCEL_KILL_ARMED = True
        try:
            child.terminate()
        except ProcessLookupError:
            pass


def _kill_cancelled_child(_signum: int, _frame: Any) -> None:
    """Escalate a cancellation if the active CLI ignored TERM."""
    child = _CURRENT_CHILD
    if child is not None and child.poll() is None:
        try:
            child.kill()
        except ProcessLookupError:
            pass


def _install_signal_handlers() -> None:
    signal.signal(signal.SIGTERM, _signal_handler)
    signal.signal(signal.SIGINT, _signal_handler)
    signal.signal(signal.SIGALRM, _kill_cancelled_child)


def _terminate_and_reap(child: subprocess.Popen[str]) -> None:
    if child.poll() is None:
        try:
            child.terminate()
        except ProcessLookupError:
            pass
    try:
        child.communicate(timeout=3.0)
    except subprocess.TimeoutExpired:
        try:
            child.kill()
        except ProcessLookupError:
            pass
        child.communicate()


def _run_cli(
    argv: Sequence[str],
    stderr_path: Path,
    *,
    capture_stdout: bool = False,
    expected_signal_generation: int | None = None,
) -> CliResult:
    """Run one CLI attempt, including startup, RPC, and post-command health checks."""
    global _CANCEL_KILL_ARMED, _CURRENT_CHILD
    stderr_path.parent.mkdir(parents=True, exist_ok=True)
    signal_generation = _SIGNAL_GENERATION
    start_ns = 0
    finished_ns = 0
    stdout = ""
    exit_code = 127
    spawned = False
    child: subprocess.Popen[str] | None = None
    with stderr_path.open("ab", buffering=0) as stderr_file:
        start_ns = time.monotonic_ns()
        signal_generation = _SIGNAL_GENERATION
        if (
            expected_signal_generation is not None
            and _SIGNAL_GENERATION != expected_signal_generation
        ):
            stderr_file.write(b"Cancelled before CLI launch\n")
            finished_ns = time.monotonic_ns()
        else:
            try:
                child = subprocess.Popen(
                    list(argv),
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE if capture_stdout else subprocess.DEVNULL,
                    stderr=stderr_file,
                    text=True,
                    encoding="utf-8",
                    errors="replace",
                )
            except OSError as error:
                stderr_file.write(
                    f"Could not start CLI: {error}\n".encode("utf-8", errors="replace")
                )
                finished_ns = time.monotonic_ns()
            else:
                spawned = True
                _CURRENT_CHILD = child
                try:
                    if _SIGNAL_GENERATION != signal_generation:
                        _terminate_and_reap(child)
                    else:
                        stdout, _ = child.communicate()
                    exit_code = child.returncode if child.returncode is not None else 125
                finally:
                    if child.poll() is None:
                        _terminate_and_reap(child)
                    _CURRENT_CHILD = None
                    signal.setitimer(signal.ITIMER_REAL, 0)
                    _CANCEL_KILL_ARMED = False
                finished_ns = time.monotonic_ns()
    duration_ns = finished_ns - start_ns
    interrupted = _SIGNAL_GENERATION != signal_generation
    if exit_code == 0:
        status = "success"
    elif interrupted or exit_code < 0:
        status = "unknown"
    else:
        status = "failure"
    return CliResult(stdout or "", exit_code, status, start_ns, duration_ns, spawned)


def _environment() -> tuple[Path, str, Path]:
    ws_root = Path(os.environ["WS_ROOT"]).resolve()
    cli = os.environ["WS_CKPT_BIN"]
    out_dir = Path(os.environ["OUT_DIR"]).resolve()
    return ws_root, cli, out_dir


def _workspace(ws_root: Path, client: int) -> Path:
    workspace = ws_root / f"ws-u{client}"
    if not workspace.is_dir():
        raise ValueError(f"workspace is missing or not a directory: {workspace}")
    return workspace


def _snapshot_id(run_id: str, client: int, iteration: int) -> str:
    snapshot_id = f"ha{run_id}-u{client}-n{iteration}"
    if len(snapshot_id) > 64 or re.fullmatch(r"[A-Za-z0-9_-]+", snapshot_id) is None:
        raise ValueError("generated snapshot ID is outside the CLI's safe ID format")
    return snapshot_id


def _active_marker(out_dir: Path, client: int) -> Path:
    return out_dir / "active" / f"client-{client}.snapshot"


def _write_marker(marker: Path, snapshot_id: str, state: str = "owned") -> None:
    marker.parent.mkdir(parents=True, exist_ok=True)
    file_descriptor, temp_name = tempfile.mkstemp(prefix=f".{marker.name}.", dir=marker.parent)
    try:
        with os.fdopen(file_descriptor, "w", encoding="utf-8") as stream:
            stream.write(snapshot_id + "\n")
            stream.write(state + "\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temp_name, marker)
        directory_fd = os.open(marker.parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    except BaseException:
        try:
            os.unlink(temp_name)
        except FileNotFoundError:
            pass
        raise


def _remove_marker(marker: Path) -> None:
    try:
        marker.unlink()
    except FileNotFoundError:
        return
    directory_fd = os.open(marker.parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(directory_fd)
    finally:
        os.close(directory_fd)


def _read_marker(marker: Path, client: int) -> tuple[str, str]:
    lines = marker.read_text(encoding="utf-8").splitlines()
    if len(lines) != 2 or lines[1] not in ("pending", "owned", "unknown"):
        raise ValueError("active marker must contain an ID and a known ownership state")
    snapshot_id = lines[0]
    valid = re.fullmatch(rf"ha[A-Za-z0-9-]{{1,24}}-u{client}-n[1-9][0-9]*", snapshot_id)
    if valid is None or len(snapshot_id) > 64:
        raise ValueError("active marker does not contain a helper-owned snapshot ID")
    return snapshot_id, lines[1]


def _manual_recovery(cli: str, workspace: Path, snapshot_id: str) -> str:
    return (
        "Inspect the workspace with `"
        + shlex.quote(cli)
        + " list --workspace "
        + shlex.quote(str(workspace))
        + " --format json`; if the exact snapshot is present, remove it with `"
        + shlex.quote(cli)
        + " delete --workspace "
        + shlex.quote(str(workspace))
        + " --snapshot "
        + shlex.quote(snapshot_id)
        + " --force`. Keep the active marker until absence is confirmed."
    )


def cleanup(client: int) -> int:
    """Clean known-owned snapshots; pending and unknown mutations require manual recovery."""
    _install_signal_handlers()
    ws_root, cli, out_dir = _environment()
    workspace = _workspace(ws_root, client)
    marker = _active_marker(out_dir, client)
    if not marker.exists():
        return 0
    try:
        snapshot_id, marker_state = _read_marker(marker, client)
    except (OSError, UnicodeError, ValueError) as error:
        print(
            f"Cannot safely read active marker {marker}: {error}; marker retained.", file=sys.stderr
        )
        return 1
    if marker_state in ("pending", "unknown"):
        print(
            f"Client {client} has a {marker_state} mutation outcome for snapshot {snapshot_id}; "
            f"marker retained and automatic cleanup is unsafe. {_manual_recovery(cli, workspace, snapshot_id)}",
            file=sys.stderr,
        )
        return 1

    log_base = out_dir / "logs" / f"client-{client}.cleanup"
    list_result = _run_cli(
        [cli, "list", "--workspace", str(workspace), "--format", "json"],
        log_base.with_suffix(".list.stderr.log"),
        capture_stdout=True,
    )
    if list_result.status != "success":
        print(
            f"Snapshot presence check failed for client {client}; marker retained. "
            f"{_manual_recovery(cli, workspace, snapshot_id)}",
            file=sys.stderr,
        )
        return 1
    try:
        items = json.loads(list_result.stdout)
        if not isinstance(items, list) or any(
            not isinstance(item, dict) or not isinstance(item.get("id"), str) for item in items
        ):
            raise ValueError("list output is not an array of objects with string IDs")
        present = any(item["id"] == snapshot_id for item in items)
    except (json.JSONDecodeError, ValueError) as error:
        print(
            f"Cannot verify snapshot presence for client {client}: {error}; marker retained. "
            f"{_manual_recovery(cli, workspace, snapshot_id)}",
            file=sys.stderr,
        )
        return 1

    if not present:
        _remove_marker(marker)
        return 0

    _write_marker(marker, snapshot_id, "pending")
    delete_result = _run_cli(
        [cli, "delete", "--workspace", str(workspace), "--snapshot", snapshot_id, "--force"],
        log_base.with_suffix(".delete.stderr.log"),
    )
    if delete_result.exit_code != 0:
        if delete_result.spawned:
            _write_marker(marker, snapshot_id, "unknown")
        else:
            _write_marker(marker, snapshot_id, "owned")
        reason = (
            "delete CLI outcome is unknown"
            if delete_result.status == "unknown"
            else "delete command failed"
        )
        print(
            f"Cleanup could not complete for client {client} ({reason}); marker retained. "
            f"{_manual_recovery(cli, workspace, snapshot_id)}",
            file=sys.stderr,
        )
        return 1
    _write_marker(marker, snapshot_id, "owned")
    verify_result = _run_cli(
        [cli, "list", "--workspace", str(workspace), "--format", "json"],
        log_base.with_suffix(".verify.stderr.log"),
        capture_stdout=True,
    )
    if verify_result.status == "success":
        try:
            remaining = json.loads(verify_result.stdout)
            if (
                isinstance(remaining, list)
                and all(
                    isinstance(item, dict) and isinstance(item.get("id"), str) for item in remaining
                )
                and not any(item["id"] == snapshot_id for item in remaining)
            ):
                _remove_marker(marker)
                return 0
        except json.JSONDecodeError:
            pass

    reason = "snapshot remains or verification failed"
    print(
        f"Cleanup could not confirm removal for client {client} ({reason}); marker retained. "
        f"{_manual_recovery(cli, workspace, snapshot_id)}",
        file=sys.stderr,
    )
    return 1


def _append_operation_row(stream: Any, row: dict[str, object]) -> None:
    writer = csv.DictWriter(
        stream, fieldnames=OPERATION_HEADER, delimiter="\t", lineterminator="\n"
    )
    writer.writerow(row)
    stream.flush()


def worker(client: int, run_id: str) -> int:
    """Run one client's checkpoint, rollback, and delete sequence."""
    _install_signal_handlers()
    if RUN_ID_PATTERN.fullmatch(run_id) is None:
        raise ValueError("RUN_ID must contain 1-24 ASCII letters, digits, or hyphens")
    iterations = int(os.environ["ITERATIONS"])
    if iterations < 1:
        raise ValueError("ITERATIONS must be a positive integer")
    ws_root, cli, out_dir = _environment()
    workspace = _workspace(ws_root, client)
    operations_dir = out_dir / "operations"
    operations_dir.mkdir(parents=True, exist_ok=True)
    operations_path = operations_dir / f"client-{client}.tsv"
    marker = _active_marker(out_dir, client)
    if marker.exists():
        raise RuntimeError(f"active marker already exists; recover it before starting: {marker}")

    worker_failed = False
    launch_stream: Any = None
    try:
        with operations_path.open("x", encoding="utf-8", newline="") as stream:
            writer = csv.DictWriter(
                stream, fieldnames=OPERATION_HEADER, delimiter="\t", lineterminator="\n"
            )
            writer.writeheader()
            launch_path = operations_dir / f"client-{client}.launch-failures.tsv"
            launch_stream = launch_path.open("x", encoding="utf-8", newline="")
            launch_writer = csv.DictWriter(
                launch_stream,
                fieldnames=(
                    "iteration",
                    "operation",
                    "start_ns",
                    "duration_ns",
                    "exit_code",
                    "diagnostic",
                ),
                delimiter="\t",
                lineterminator="\n",
            )
            launch_writer.writeheader()
            for iteration in range(1, iterations + 1):
                if _SIGNAL_GENERATION:
                    worker_failed = True
                    break
                snapshot_id = _snapshot_id(run_id, client, iteration)
                previous_state = "owned" if marker.exists() else None
                for operation, argv in (
                    (
                        "checkpoint",
                        [
                            cli,
                            "checkpoint",
                            "--workspace",
                            str(workspace),
                            "--snapshot",
                            snapshot_id,
                            "--message",
                            "x",
                        ],
                    ),
                    (
                        "rollback",
                        [cli, "rollback", "--workspace", str(workspace), "--num-ancestors", "1"],
                    ),
                    (
                        "delete",
                        [
                            cli,
                            "delete",
                            "--workspace",
                            str(workspace),
                            "--snapshot",
                            snapshot_id,
                            "--force",
                        ],
                    ),
                ):
                    if _SIGNAL_GENERATION:
                        worker_failed = True
                        break
                    operation_signal_generation = _SIGNAL_GENERATION
                    _write_marker(marker, snapshot_id, "pending")
                    if _SIGNAL_GENERATION != operation_signal_generation:
                        if previous_state is None:
                            _remove_marker(marker)
                        else:
                            _write_marker(marker, snapshot_id, previous_state)
                        worker_failed = True
                        break
                    stderr_path = (
                        out_dir
                        / "logs"
                        / f"client-{client}-iteration-{iteration}-{operation}.stderr.log"
                    )
                    result = _run_cli(
                        argv,
                        stderr_path,
                        expected_signal_generation=operation_signal_generation,
                    )
                    if not result.spawned:
                        if previous_state is None:
                            _remove_marker(marker)
                        else:
                            _write_marker(marker, snapshot_id, previous_state)
                        launch_writer.writerow(
                            {
                                "iteration": iteration,
                                "operation": operation,
                                "start_ns": result.start_ns,
                                "duration_ns": result.duration_ns,
                                "exit_code": result.exit_code,
                                "diagnostic": "CLI process was not started; see matching stderr log",
                            }
                        )
                        launch_stream.flush()
                        worker_failed = True
                        break
                    _append_operation_row(
                        stream,
                        {
                            "iteration": iteration,
                            "operation": operation,
                            "start_ns": result.start_ns,
                            "duration_ns": result.duration_ns,
                            "exit_code": result.exit_code,
                            "status": result.status,
                        },
                    )
                    if result.status != "success":
                        if result.spawned:
                            _write_marker(marker, snapshot_id, "unknown")
                        elif previous_state is None:
                            _remove_marker(marker)
                        else:
                            _write_marker(marker, snapshot_id, previous_state)
                        worker_failed = True
                        break
                    if operation == "delete":
                        _remove_marker(marker)
                        previous_state = None
                    else:
                        _write_marker(marker, snapshot_id, "owned")
                        previous_state = "owned"
                    if _SIGNAL_GENERATION:
                        worker_failed = True
                        break
                if worker_failed:
                    break
    finally:
        if launch_stream is not None:
            launch_stream.close()
        if marker.exists() and cleanup(client) != 0:
            worker_failed = True

    if worker_failed:
        return 128 + _LAST_SIGNAL if _LAST_SIGNAL else 1
    return 0


def _nearest_rank(values: list[int], percentile: int) -> int:
    ordered = sorted(values)
    return ordered[max(0, math.ceil(percentile * len(ordered) / 100) - 1)]


def summarize(
    out_dir: Path,
    clients: int,
    iterations: int,
    started_ns: int,
    ended_ns: int,
    worker_failures: int,
    interrupted: bool,
) -> int:
    """Write business latency and throughput summaries for the full run window."""
    reasons: list[str] = []
    if clients < 1 or iterations < 1:
        reasons.append("clients and iterations must be positive")
    window_ns = ended_ns - started_ns
    if window_ns <= 0:
        reasons.append("measurement window is not positive")
    if worker_failures != 0:
        reasons.append("one or more workers failed")
    if interrupted:
        reasons.append("run was interrupted")

    rows: list[dict[str, str]] = []
    for client in range(1, clients + 1):
        path = out_dir / "operations" / f"client-{client}.tsv"
        try:
            with path.open("r", encoding="utf-8", newline="") as stream:
                reader = csv.DictReader(stream, delimiter="\t")
                if tuple(reader.fieldnames or ()) != OPERATION_HEADER:
                    reasons.append(f"invalid operation header for client {client}")
                    continue
                for row in reader:
                    if None in row or any(row.get(field) is None for field in OPERATION_HEADER):
                        reasons.append(f"malformed operation row for client {client}")
                        continue
                    try:
                        iteration = int(row["iteration"])
                        duration_ns = int(row["duration_ns"])
                        start_ns = int(row["start_ns"])
                        exit_code = int(row["exit_code"])
                    except (TypeError, ValueError):
                        reasons.append(f"non-numeric operation data for client {client}")
                        continue
                    if (
                        iteration < 1
                        or iteration > iterations
                        or duration_ns < 0
                        or start_ns < started_ns
                        or start_ns + duration_ns > ended_ns
                        or row["operation"] not in OPERATION_NAMES
                        or row["status"] not in OUTCOME_NAMES
                        or (row["status"] == "success") != (exit_code == 0)
                    ):
                        reasons.append(
                            f"operation data is outside the run window or inconsistent for client {client}"
                        )
                        continue
                    rows.append({**row, "client": str(client)})
        except OSError as error:
            reasons.append(f"cannot read client {client} operations: {error}")

    expected_attempts = clients * iterations * len(OPERATION_NAMES)
    attempts = len(rows)
    launch_failures = 0
    for client in range(1, clients + 1):
        path = out_dir / "operations" / f"client-{client}.launch-failures.tsv"
        if not path.exists():
            continue
        try:
            with path.open("r", encoding="utf-8", newline="") as stream:
                reader = csv.DictReader(stream, delimiter="\t")
                expected_header = (
                    "iteration",
                    "operation",
                    "start_ns",
                    "duration_ns",
                    "exit_code",
                    "diagnostic",
                )
                if tuple(reader.fieldnames or ()) != expected_header:
                    reasons.append(f"invalid launch-failure header for client {client}")
                    continue
                for row in reader:
                    if None in row or any(row.get(field) is None for field in expected_header):
                        reasons.append(f"malformed launch-failure row for client {client}")
                        continue
                    launch_failures += 1
        except OSError as error:
            reasons.append(f"cannot read client {client} launch failures: {error}")
    if launch_failures:
        reasons.append("one or more CLI processes could not be started")
    successes = sum(row["status"] == "success" for row in rows)
    failures = sum(row["status"] == "failure" for row in rows)
    unknown_attempts = sum(row["status"] == "unknown" for row in rows)
    if failures:
        reasons.append("one or more CLI calls returned a nonzero failure")
    if unknown_attempts:
        reasons.append("one or more CLI mutation outcomes are unknown")
    skipped = max(0, expected_attempts - attempts)
    if attempts != expected_attempts:
        reasons.append("attempt count differs from planned operations")

    by_cycle: dict[tuple[int, int], list[dict[str, str]]] = {}
    for row in rows:
        by_cycle.setdefault((int(row["client"]), int(row["iteration"])), []).append(row)
    completed_cycles = 0
    for client in range(1, clients + 1):
        for iteration in range(1, iterations + 1):
            cycle = by_cycle.get((client, iteration), [])
            if len(cycle) == 3 and {row["operation"] for row in cycle} == set(OPERATION_NAMES):
                if all(row["status"] == "success" for row in cycle):
                    completed_cycles += 1
    expected_cycles = clients * iterations
    if completed_cycles != expected_cycles:
        reasons.append("not all planned cycles completed successfully")

    latency_path = out_dir / "business-latency.tsv"
    latency_path.parent.mkdir(parents=True, exist_ok=True)
    with latency_path.open("w", encoding="utf-8", newline="") as stream:
        writer = csv.DictWriter(
            stream, fieldnames=LATENCY_HEADER, delimiter="\t", lineterminator="\n"
        )
        writer.writeheader()
        for operation in OPERATION_NAMES:
            for outcome in OUTCOME_NAMES:
                values = [
                    int(row["duration_ns"])
                    for row in rows
                    if row["operation"] == operation and row["status"] == outcome
                ]
                writer.writerow(
                    {
                        "operation": operation,
                        "outcome": outcome,
                        "count": len(values),
                        "p50_ns": _nearest_rank(values, 50) if values else "",
                        "p95_ns": _nearest_rank(values, 95) if values else "",
                        "p99_ns": _nearest_rank(values, 99) if values else "",
                        "max_ns": max(values) if values else "",
                    }
                )

    active_dir = out_dir / "active"
    active_markers = sorted(active_dir.glob("client-*.snapshot")) if active_dir.exists() else []
    unresolved_mutations = 0
    if active_markers:
        reasons.append("active snapshot markers remain after cleanup")
    for marker in active_markers:
        try:
            marker_lines = marker.read_text(encoding="utf-8").splitlines()
        except (OSError, UnicodeError):
            unresolved_mutations += 1
            continue
        if len(marker_lines) != 2 or marker_lines[1] in ("pending", "unknown"):
            unresolved_mutations += 1
    if unresolved_mutations:
        reasons.append("unresolved snapshot mutations remain in active markers")
    if successes + failures + unknown_attempts != attempts:
        reasons.append("operation outcome counts do not equal attempts")

    valid = not reasons
    metrics: list[tuple[str, object]] = [
        ("status", "valid" if valid else "invalid"),
        ("clients", clients),
        ("iterations", iterations),
        ("measurement_window_ns", window_ns),
        ("worker_failures", worker_failures),
        ("interrupted", int(interrupted)),
        ("attempts", attempts),
        (
            "attempts_per_second",
            f"{attempts * 1_000_000_000 / window_ns:.6f}" if window_ns > 0 else "",
        ),
        ("launch_failures", launch_failures),
        ("successes", successes),
        ("failures", failures),
        ("unknown_attempts", unknown_attempts),
        ("unresolved_mutations", unresolved_mutations),
        ("skipped", skipped),
        ("completed_cycles", completed_cycles),
        ("ops_per_second", f"{successes * 1_000_000_000 / window_ns:.6f}" if window_ns > 0 else ""),
        (
            "cycles_per_second",
            f"{completed_cycles * 1_000_000_000 / window_ns:.6f}" if window_ns > 0 else "",
        ),
        ("active_markers_remaining", len(active_markers)),
        ("invalid_reasons", "; ".join(dict.fromkeys(reasons))),
    ]
    with (out_dir / "business-summary.tsv").open("w", encoding="utf-8", newline="") as stream:
        writer = csv.writer(stream, delimiter="\t", lineterminator="\n")
        writer.writerow(SUMMARY_HEADER)
        writer.writerows(metrics)
    return 0 if valid else 1


def _positive_int(value: str) -> int:
    parsed = int(value)
    if parsed < 1:
        raise argparse.ArgumentTypeError("must be a positive integer")
    return parsed


def _nonnegative_int(value: str) -> int:
    parsed = int(value)
    if parsed < 0:
        raise argparse.ArgumentTypeError("must be a non-negative integer")
    return parsed


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    worker_parser = subparsers.add_parser("worker", help="run one client's measured workload")
    worker_parser.add_argument("client", type=_positive_int)
    worker_parser.add_argument("run_id")
    cleanup_parser = subparsers.add_parser("cleanup", help="clean this client's marked snapshot")
    cleanup_parser.add_argument("client", type=_positive_int)
    summary_parser = subparsers.add_parser("summarize", help="summarize all worker measurements")
    summary_parser.add_argument("--out-dir", required=True, type=Path)
    summary_parser.add_argument("--clients", required=True, type=_positive_int)
    summary_parser.add_argument("--iterations", required=True, type=_positive_int)
    summary_parser.add_argument("--started-ns", required=True, type=int)
    summary_parser.add_argument("--ended-ns", required=True, type=int)
    summary_parser.add_argument("--worker-failures", required=True, type=_nonnegative_int)
    summary_parser.add_argument("--interrupted", required=True, choices=("0", "1"))
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = _build_parser()
    args = parser.parse_args(argv)
    try:
        if args.command == "worker":
            return worker(args.client, args.run_id)
        if args.command == "cleanup":
            return cleanup(args.client)
        return summarize(
            args.out_dir.resolve(),
            args.clients,
            args.iterations,
            args.started_ns,
            args.ended_ns,
            args.worker_failures,
            args.interrupted == "1",
        )
    except (KeyError, OSError, ValueError, RuntimeError) as error:
        print(f"health advisory metrics helper failed: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
