#!/usr/bin/env python3
"""Sample Linux process resources and optional AgentSight internal metrics."""

from __future__ import annotations

import argparse
import csv
import json
import os
import shutil
import signal
import time
from pathlib import Path

FIELDS = (
    "timestamp",
    "input_qps",
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
INTERNAL_FIELDS = {
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
}
PROMETHEUS_FIELDS = {
    "agentsight_connection_cache_bytes": "connection_cache_bytes",
    "agentsight_event_channel_length": "channel_length",
    "agentsight_event_channel_bytes": "event_channel_bytes",
    "agentsight_event_channel_budget_bytes": "event_channel_budget_bytes",
    "agentsight_pending_genai_count": "pending_genai_count",
    "agentsight_pending_genai_bytes": "pending_genai_bytes",
    "agentsight_pending_connection_count": "pending_connection_count",
    "agentsight_pending_connection_bytes": "pending_connection_bytes",
    "agentsight_connection_evictions_total": "eviction_count",
    "agentsight_ring_buffer_dropped_total": "ring_buffer_dropped",
    "agentsight_channel_dropped_total": "channel_dropped",
    "agentsight_events_completed_total": "completed",
}
STOP = False
MIB = 1024 * 1024
GIB = 1024 * MIB
SAFETY_EXIT_CODE = 75
STORAGE_SAFETY_INTERVAL_SECONDS = 10.0


def stop(_: int, __: object) -> None:
    """Stop after the current sample so the CSV remains valid."""
    global STOP
    STOP = True


def read_status(pid: int) -> dict[str, int]:
    """Read numeric process fields from procfs."""
    values: dict[str, int] = {}
    try:
        text = Path(f"/proc/{pid}/status").read_text(encoding="utf-8", errors="replace")
    except OSError:
        return values
    for line in text.splitlines():
        key, separator, remainder = line.partition(":")
        if not separator:
            continue
        first = remainder.strip().split(maxsplit=1)[0] if remainder.strip() else ""
        if key in {"VmRSS", "VmHWM", "VmSize", "Threads"}:
            try:
                values[key] = int(first)
            except ValueError:
                values[key] = 0
    return values


def read_cpu_ticks(pid: int) -> int:
    """Return user plus system CPU ticks for a process."""
    try:
        text = Path(f"/proc/{pid}/stat").read_text(encoding="utf-8")
        fields = text[text.rfind(")") + 2 :].split()
        return int(fields[11]) + int(fields[12])
    except (OSError, IndexError, ValueError):
        return 0


def fd_count(pid: int) -> int:
    """Count open descriptors, returning zero after process exit."""
    try:
        return len(list(Path(f"/proc/{pid}/fd").iterdir()))
    except OSError:
        return 0


def socket_count(pid: int) -> int:
    """Count socket descriptors owned by a process."""
    try:
        return sum(
            1
            for fd in Path(f"/proc/{pid}/fd").iterdir()
            if fd.is_symlink() and os.readlink(fd).startswith("socket:")
        )
    except OSError:
        return 0


def read_internal_metrics(path: Path | None) -> dict[str, str]:
    """Read current Prometheus snapshots and legacy ``key=value`` exports."""
    if path is None:
        return {}
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError:
        return {}
    result: dict[str, str] = {}
    for line in lines:
        fields = line.split()
        if len(fields) == 2 and fields[0] in PROMETHEUS_FIELDS:
            result[PROMETHEUS_FIELDS[fields[0]]] = fields[1]
            continue
        key, separator, value = line.partition("=")
        if separator and key in INTERNAL_FIELDS:
            result[key] = value.strip()
    return result


def allocated_size(path: Path) -> int:
    """Return allocated bytes without following links outside the result tree."""
    total = 0
    pending = [path]
    while pending:
        current = pending.pop()
        try:
            entries = list(os.scandir(current))
        except OSError:
            continue
        for entry in entries:
            try:
                stat = entry.stat(follow_symlinks=False)
            except OSError:
                continue
            total += stat.st_blocks * 512
            if entry.is_dir(follow_symlinks=False):
                pending.append(Path(entry.path))
    return total


def available_memory_bytes() -> int | None:
    """Read Linux MemAvailable, which accounts for reclaimable page cache."""
    try:
        lines = Path("/proc/meminfo").read_text(encoding="utf-8").splitlines()
    except OSError:
        return None
    for line in lines:
        if line.startswith("MemAvailable:"):
            try:
                return int(line.split()[1]) * 1024
            except (IndexError, ValueError):
                return None
    return None


def safety_violation(
    results_root: Path,
    agent_rss_mb: float | None,
    max_results_gb: int,
    min_free_disk_gb: int,
    min_available_memory_mb: int,
    max_agentsight_rss_mb: int,
    check_storage: bool = True,
) -> dict[str, object] | None:
    """Return an actionable safety record before disk or memory is exhausted."""
    result_bytes = allocated_size(results_root) if check_storage else None
    max_result_bytes = max_results_gb * GIB
    # Stop at 95% so writes between one-second samples cannot cross the hard budget.
    result_stop_bytes = max_result_bytes - max(max_result_bytes // 20, 256 * MIB)
    free_disk_bytes = shutil.disk_usage(results_root).free if check_storage else None
    available_memory = available_memory_bytes()
    checks = (
        (
            result_bytes is not None and result_bytes >= result_stop_bytes,
            "results_budget",
            result_bytes,
            result_stop_bytes,
        ),
        (
            free_disk_bytes is not None and free_disk_bytes < min_free_disk_gb * GIB,
            "free_disk",
            free_disk_bytes,
            min_free_disk_gb * GIB,
        ),
        (
            available_memory is not None
            and available_memory < min_available_memory_mb * MIB,
            "available_memory",
            available_memory,
            min_available_memory_mb * MIB,
        ),
        (
            agent_rss_mb is not None and agent_rss_mb > max_agentsight_rss_mb,
            "agentsight_rss",
            agent_rss_mb,
            max_agentsight_rss_mb,
        ),
    )
    for failed, reason, observed, limit in checks:
        if failed:
            return {
                "schema_version": 1,
                "reason": reason,
                "observed": observed,
                "limit": limit,
                "results_bytes": result_bytes,
                "free_disk_bytes": free_disk_bytes,
                "available_memory_bytes": available_memory,
                "agentsight_rss_mb": agent_rss_mb,
                "stopped_at_unix": time.time(),
            }
    return None


def write_json(path: Path, value: dict[str, object]) -> None:
    """Atomically save a safety stop record."""
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    temporary.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    temporary.replace(path)


def sample(
    pid: int,
    previous_ticks: int,
    previous_time: float,
    input_qps: float,
    metrics: Path | None,
) -> tuple[dict[str, object], int, float]:
    """Collect one row and the state needed for the next CPU calculation."""
    now = time.monotonic()
    status = read_status(pid)
    alive = bool(status)
    ticks = read_cpu_ticks(pid) if alive else previous_ticks
    elapsed = max(now - previous_time, 1e-9)
    cpu = (
        max(
            0.0,
            (ticks - previous_ticks)
            / os.sysconf(os.sysconf_names["SC_CLK_TCK"])
            / elapsed
            * 100,
        )
        if alive
        else None
    )
    row: dict[str, object] = {
        "timestamp": time.time(),
        "input_qps": input_qps,
        "process_alive": int(alive),
        "cpu_pct": f"{cpu:.3f}" if cpu is not None else "",
        "rss_mb": f"{status['VmRSS'] / 1024:.3f}" if "VmRSS" in status else "",
        "threads": status.get("Threads", ""),
        "file_descriptors": fd_count(pid) if alive else "",
        "active_connections": socket_count(pid) if alive else "",
        "connection_cache_bytes": "",
        "channel_length": "",
        "event_channel_bytes": "",
        "event_channel_budget_bytes": "",
        "pending_genai_count": "",
        "pending_genai_bytes": "",
        "pending_connection_count": "",
        "pending_connection_bytes": "",
        "eviction_count": "",
        "ring_buffer_dropped": "",
        "channel_dropped": "",
        "completed": "",
    }
    row.update(read_internal_metrics(metrics))
    return row, ticks, now


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pid", type=int, required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--interval", type=float, default=1.0)
    parser.add_argument(
        "--duration", type=float, default=0, help="0 means until signalled"
    )
    parser.add_argument("--input-qps", type=float, default=0)
    parser.add_argument(
        "--metrics-file",
        type=Path,
        help="optional Prometheus or legacy key=value internal metrics snapshot",
    )
    parser.add_argument("--load-pid", type=int, help="load process stopped by a guard")
    parser.add_argument("--results-root", type=Path)
    parser.add_argument("--safety-output", type=Path)
    parser.add_argument("--max-results-gb", type=int, default=30)
    parser.add_argument("--min-free-disk-gb", type=int, default=5)
    parser.add_argument("--min-available-memory-mb", type=int, default=2048)
    parser.add_argument("--max-agentsight-rss-mb", type=int, default=1536)
    args = parser.parse_args()
    if args.interval <= 0:
        raise SystemExit("interval must be positive")
    for name in (
        "max_results_gb",
        "min_free_disk_gb",
        "min_available_memory_mb",
        "max_agentsight_rss_mb",
    ):
        if getattr(args, name) <= 0:
            raise SystemExit(f"{name.replace('_', '-')} must be positive")
    safety_values = (args.load_pid, args.results_root, args.safety_output)
    if any(value is not None for value in safety_values) and not all(
        value is not None for value in safety_values
    ):
        raise SystemExit(
            "load-pid, results-root, and safety-output must be provided together"
        )
    if args.load_pid is not None and args.load_pid <= 0:
        raise SystemExit("load-pid must be positive")
    if args.results_root is not None and not args.results_root.is_dir():
        raise SystemExit(f"results-root does not exist: {args.results_root}")
    if not Path(f"/proc/{args.pid}").exists():
        raise SystemExit(f"process {args.pid} does not exist")
    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)
    started = time.monotonic()
    previous_time = started
    previous_ticks = read_cpu_ticks(args.pid)
    last_storage_check: float | None = None
    with open(args.output, "w", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=FIELDS)
        writer.writeheader()
        while not STOP and (
            args.duration <= 0 or time.monotonic() - started < args.duration
        ):
            row, previous_ticks, previous_time = sample(
                args.pid,
                previous_ticks,
                previous_time,
                args.input_qps,
                args.metrics_file,
            )
            writer.writerow(row)
            handle.flush()
            if (
                args.load_pid is not None
                and args.results_root is not None
                and args.safety_output is not None
            ):
                rss_text = row.get("rss_mb")
                rss_mb = float(rss_text) if rss_text not in (None, "") else None
                now = time.monotonic()
                check_storage = (
                    last_storage_check is None
                    or now - last_storage_check >= STORAGE_SAFETY_INTERVAL_SECONDS
                )
                violation = safety_violation(
                    args.results_root,
                    rss_mb,
                    args.max_results_gb,
                    args.min_free_disk_gb,
                    args.min_available_memory_mb,
                    args.max_agentsight_rss_mb,
                    check_storage,
                )
                if check_storage:
                    last_storage_check = now
                if violation is not None:
                    write_json(args.safety_output, violation)
                    try:
                        os.kill(args.load_pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                    return SAFETY_EXIT_CODE
            time.sleep(args.interval)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
