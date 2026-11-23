#!/usr/bin/env python3
"""Run independent test commands in parallel with buffered output."""

from __future__ import annotations

import argparse
import concurrent.futures
import os
import shlex
import subprocess
import sys
import time


def _default_jobs() -> int:
    raw = os.environ.get("TOKENLESS_TEST_JOBS")
    if raw:
        try:
            parsed = int(raw)
            if parsed > 0:
                return parsed
        except ValueError:
            pass
    return min(os.cpu_count() or 4, 8)


def _run_one(cmd: str) -> tuple[str, int, float, str, str]:
    argv = shlex.split(cmd)
    if not argv:
        return (cmd, 2, 0.0, "", "empty command\n")
    started = time.monotonic()
    proc = subprocess.run(argv, capture_output=True, text=True)
    elapsed = time.monotonic() - started
    return (cmd, proc.returncode, elapsed, proc.stdout, proc.stderr)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--jobs",
        type=int,
        default=_default_jobs(),
        help="Maximum concurrent test commands",
    )
    parser.add_argument(
        "commands",
        nargs="+",
        help="Shell-quoted commands to execute (after --)",
    )
    args = parser.parse_args(argv)
    if args.jobs < 1:
        parser.error("--jobs must be a positive integer")

    failures: list[tuple[str, int, str, str]] = []
    max_workers = min(args.jobs, len(args.commands))
    with concurrent.futures.ThreadPoolExecutor(max_workers=max_workers) as pool:
        future_map = [pool.submit(_run_one, cmd) for cmd in args.commands]
        for future in concurrent.futures.as_completed(future_map):
            cmd, rc, elapsed, stdout, stderr = future.result()
            if rc == 0:
                print(f"PASS [{elapsed:.2f}s] {cmd}", flush=True)
                if stdout:
                    print(stdout, end="" if stdout.endswith("\n") else "\n", flush=True)
                if stderr:
                    print(stderr, file=sys.stderr, end="" if stderr.endswith("\n") else "\n", flush=True)
            else:
                print(f"FAIL [{elapsed:.2f}s] (rc={rc}) {cmd}", flush=True)
                failures.append((cmd, rc, stdout, stderr))

    if failures:
        for cmd, rc, stdout, stderr in failures:
            print(f"\n=== FAILED (rc={rc}): {cmd} ===", file=sys.stderr)
            if stdout:
                print("--- stdout ---", file=sys.stderr)
                print(stdout, file=sys.stderr, end="" if stdout.endswith("\n") else "\n")
            if stderr:
                print("--- stderr ---", file=sys.stderr)
                print(stderr, file=sys.stderr, end="" if stderr.endswith("\n") else "\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
