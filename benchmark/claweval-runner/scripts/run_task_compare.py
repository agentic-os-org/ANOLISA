#!/usr/bin/env python3

# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Run a given task in both native (claw-eval) and ce-runner modes, producing trace files.

Supports single-task mode and batch parallel mode.

Usage:
    # Single-task mode (default)
    python scripts/run_task_compare.py T009zh_contact_lookup
    python scripts/run_task_compare.py T009zh_contact_lookup --mode native
    python scripts/run_task_compare.py M019_doc_extraction_radar_chart --sandbox

    # Batch parallel mode
    python scripts/run_task_compare.py --batch --tasks T001zh_email_triage T002zh_invoice
    python scripts/run_task_compare.py --batch --range 1-10 --prefix T
    python scripts/run_task_compare.py --batch --tag general --parallel 8
    python scripts/run_task_compare.py --batch --tasks-file tests/input_tasks.txt

    # Specify config file
    python scripts/run_task_compare.py T009zh_contact_lookup --config config_general.yaml
"""

import argparse
import os
import subprocess
import sys
import time
from pathlib import Path

# Repo root
REPO_ROOT = Path(__file__).resolve().parent.parent
CLAW_EVAL_DIR = REPO_ROOT / "claw-eval"
TASKS_DIR = CLAW_EVAL_DIR / "tasks"


def find_task_dir(task_id: str) -> Path | None:
    """Find task directory by task ID."""
    # Try exact match
    task_dir = TASKS_DIR / task_id
    if task_dir.is_dir() and (task_dir / "task.yaml").exists():
        return task_dir

    # Try fuzzy match (case-insensitive)
    for d in TASKS_DIR.iterdir():
        if d.is_dir() and d.name.lower() == task_id.lower():
            return d

    return None


def _resolve_config(config: str | None) -> str | None:
    """Resolve config path to absolute, falling back to config_general.yaml."""
    if config:
        return str(Path(config).resolve())
    default = CLAW_EVAL_DIR / "config_general.yaml"
    return str(default) if default.exists() else None


def run_native(task_dir: Path, config: str | None = None,
               sandbox: bool = False, sandbox_image: str | None = None) -> Path | None:
    """Run task using native claw-eval CLI.

    Returns trace file path or None on failure.
    """
    cmd = [
        "claw-eval", "run",
        "--task", str(task_dir),
    ]

    resolved_config = _resolve_config(config)
    if resolved_config:
        cmd.extend(["--config", resolved_config])

    if sandbox:
        cmd.append("--sandbox")
        if sandbox_image:
            cmd.extend(["--sandbox-image", sandbox_image])

    print(f"\n{'='*60}")
    print(f"  [native] Running: {' '.join(cmd)}")
    print(f"{'='*60}")

    result = subprocess.run(cmd, cwd=str(CLAW_EVAL_DIR))

    if result.returncode != 0:
        print(f"\n[native] ❌ Failed with exit code {result.returncode}")
        return None

    return _find_latest_trace(task_dir.name, dir_pattern=None)


def run_ce_runner(task_dir: Path, config: str | None = None,
                  sandbox: bool = False, sandbox_image: str | None = None,
                  timeout: int = 600) -> Path | None:
    """Run task using ce-runner.

    Returns trace file path or None on failure.
    """
    cmd = [
        "ce-runner", "run",
        str(task_dir),
        "--timeout", str(timeout),
    ]

    resolved_config = _resolve_config(config)
    if resolved_config:
        cmd.extend(["--config", resolved_config])

    # ce-runner is always-sandbox; pass only --sandbox-image when caller customizes it
    if sandbox_image:
        cmd.extend(["--sandbox-image", sandbox_image])

    print(f"\n{'='*60}")
    print(f"  [ce-runner] Running: {' '.join(cmd)}")
    print(f"{'='*60}")

    result = subprocess.run(cmd, cwd=str(REPO_ROOT))

    if result.returncode != 0:
        print(f"\n[ce-runner] ❌ Failed with exit code {result.returncode}")
        return None

    return _find_latest_trace(task_dir.name, dir_pattern=None)


def _find_latest_trace(task_id: str, dir_pattern: str | None) -> Path | None:
    """Find the latest trace file matching task_id in traces directory.

    Args:
        task_id: Task ID to match in filename
        dir_pattern: If set, only search directories starting with this prefix.
            None means search all directories.
    """
    traces_dir = CLAW_EVAL_DIR / "traces"
    if not traces_dir.exists():
        return None

    candidates = []
    for d in traces_dir.iterdir():
        if not d.is_dir():
            continue
        if dir_pattern and not d.name.startswith(dir_pattern):
            continue

        for f in d.glob(f"*{task_id}*.jsonl"):
            if f.is_file():
                candidates.append((f.stat().st_mtime, f))

    if not candidates:
        return None

    candidates.sort(key=lambda x: x[0], reverse=True)
    return candidates[0][1]


def _discover_task_dirs(task_ids: list[str]) -> list[Path]:
    """Resolve a list of task IDs to existing task directories."""
    dirs = []
    for tid in task_ids:
        td = find_task_dir(tid)
        if td:
            dirs.append(td)
    return dirs


def run_native_batch(task_ids: list[str], config: str | None = None,
                     sandbox: bool = False, sandbox_image: str | None = None,
                     timeout: int = 600, parallel: int = 4,
                     trials: int = 1,
                     tag: str = None, range_str: str = None,
                     prefix: str = None, filter_str: str = None,
                     tasks_file: str = None,
                     tasks_dir: str = None) -> dict[str, Path | None]:
    """Run native claw-eval batch mode.

    Uses only parameters supported by both claw-eval and ce-runner:
    --filter, --tag, --range, --trials

    Returns dict mapping task_id -> trace_path.
    """
    cmd = [
        "claw-eval", "batch",
        "--parallel", str(parallel),
        "--trials", str(trials),
    ]
    if sandbox:
        cmd.append("--sandbox")
        if sandbox_image:
            cmd.extend(["--sandbox-image", sandbox_image])

    resolved_config = _resolve_config(config)
    if resolved_config:
        cmd.extend(["--config", resolved_config])

    # Task selection - use only mutually supported parameters
    if tasks_dir:
        cmd.extend(["--tasks-dir", tasks_dir])
    elif filter_str:
        cmd.extend(["--filter", filter_str])
    elif tag:
        cmd.extend(["--tag", tag])
    elif prefix:
        cmd.extend(["--prefix", prefix])
    elif range_str:
        cmd.extend(["--range", range_str])
    elif task_ids:
        # Derive --filter from task IDs (substring match, may match MORE
        # tasks than listed; acceptable for integration testing where exact
        # task selection is not required).
        common = os.path.commonprefix(task_ids)
        if len(common) >= 2:
            cmd.extend(["--filter", common])
            print(f"[native-batch] ℹ️  --filter={common!r} (substring match; may include more tasks than {len(task_ids)} listed)")
        else:
            print(f"\n[native-batch] ⚠️  Cannot derive filter from task IDs (no common prefix)")
            return {tid: None for tid in task_ids}

    print(f"\n{'='*60}")
    print(f"  [native-batch] Running: {' '.join(cmd)}")
    print(f"{'='*60}")

    result = subprocess.run(cmd, cwd=str(CLAW_EVAL_DIR))
    if result.returncode != 0:
        print(f"\n[native-batch] ❌ Failed with exit code {result.returncode}")
        return {tid: None for tid in task_ids}

    # Find traces for each task
    results = {}
    for tid in task_ids:
        results[tid] = _find_latest_trace(tid, dir_pattern=None)
    return results


def run_ce_runner_batch(task_ids: list[str], config: str | None = None,
                        sandbox: bool = False, sandbox_image: str | None = None,
                        timeout: int = 600, parallel: int = 4,
                        trials: int = 1,
                        tag: str = None, range_str: str = None,
                        prefix: str = None, filter_str: str = None,
                        tasks_file: str = None) -> dict[str, Path | None]:
    """Run ce-runner batch mode.

    Returns dict mapping task_id -> trace_path.
    """
    cmd = [
        "ce-runner", "batch",
        "--parallel", str(parallel),
        "--timeout", str(timeout),
        "--trials", str(trials),
    ]
    # ce-runner is always-sandbox; only forward --sandbox-image when caller customizes it
    if sandbox_image:
        cmd.extend(["--sandbox-image", sandbox_image])

    resolved_config = _resolve_config(config)
    if resolved_config:
        cmd.extend(["--config", resolved_config])

    # Task selection - align with run_native_batch: both sides use --filter
    # (substring) derived from task_ids commonprefix so that the two CLIs
    # execute the SAME task set. claw-eval batch only supports --filter/
    # --tag/--range and we explicitly do not pursue exact task selection
    # for integration testing.
    if tasks_file:
        cmd.extend(["--tasks-file", tasks_file])
    elif range_str:
        cmd.extend(["--range", range_str])
        if prefix:
            cmd.extend(["--prefix", prefix])
    elif tag:
        cmd.extend(["--tag", tag])
    elif prefix:
        cmd.extend(["--prefix", prefix])
    elif filter_str:
        cmd.extend(["--filter", filter_str])
    elif task_ids:
        # Derive --filter from task IDs (substring match, may match MORE
        # tasks than listed — acceptable for integration testing).
        common = os.path.commonprefix(task_ids)
        if len(common) >= 2:
            cmd.extend(["--filter", common])
            print(f"[ce-runner-batch] ℹ️  --filter={common!r} (substring match; may include more tasks than {len(task_ids)} listed)")
        else:
            print(f"\n[ce-runner-batch] ⚠️  Cannot derive filter from task IDs (no common prefix)")
            return {tid: None for tid in task_ids}

    print(f"\n{'='*60}")
    print(f"  [ce-runner-batch] Running: {' '.join(cmd)}")
    print(f"{'='*60}")

    result = subprocess.run(cmd, cwd=str(REPO_ROOT))
    if result.returncode != 0:
        print(f"\n[ce-runner-batch] ❌ Failed with exit code {result.returncode}")
        return {tid: None for tid in task_ids}

    # Find traces for each task
    results = {}
    for tid in task_ids:
        results[tid] = _find_latest_trace(tid, dir_pattern=None)
    return results


def main():
    parser = argparse.ArgumentParser(
        description="Run tasks in both native and ce-runner modes for comparison",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Single-task mode (default):
  %(prog)s T009zh_contact_lookup
  %(prog)s T009zh_contact_lookup --mode native

Batch mode:
  %(prog)s --batch
  %(prog)s --batch --tasks T001zh_email_triage T002zh_invoice --parallel 4
  %(prog)s --batch --range 1-10 --prefix T
  %(prog)s --batch --tag general
  %(prog)s --batch --tasks-file tests/input_tasks.txt
""",
    )
    parser.add_argument("task", nargs="?", default=None,
                        help="Task ID for single mode (e.g. T009zh_contact_lookup)")
    parser.add_argument("--mode", choices=["native", "ce-runner", "both"],
                        default="both",
                        help="Which mode to run (default: both)")
    parser.add_argument("--batch", action="store_true",
                        help="Run in batch mode (parallel)")
    parser.add_argument("--tasks", nargs="+", default=None,
                        help="List of task IDs for batch mode")
    parser.add_argument("--range", dest="range_str", default=None,
                        help="Numeric ID range for batch (e.g. '1-10')")
    parser.add_argument("--prefix", default=None,
                        help="Task prefix for batch (e.g. 'T', 'C', 'M')")
    parser.add_argument("--tag", default=None,
                        help="Task tag filter for batch (e.g. 'general')")
    parser.add_argument("--filter", dest="filter_str", default=None,
                        help="Substring filter for batch")
    parser.add_argument("--tasks-file", default=None,
                        help="File with task IDs (one per line) for batch")
    parser.add_argument("--parallel", type=int, default=4,
                        help="Parallel workers for batch (default: 4)")
    parser.add_argument("--sandbox", action="store_true",
                        help="Use Docker sandbox mode")
    parser.add_argument("--sandbox-image", default=None,
                        help="Docker image for sandbox")
    parser.add_argument("--timeout", type=int, default=600,
                        help="Agent timeout in seconds")
    parser.add_argument("--config", default=None,
                        help="Path to config.yaml")

    args = parser.parse_args()

    # ── Batch mode ────────────────────────────────────────────────────────
    if args.batch:
        return _run_batch_mode(args)

    # ── Single-task mode ──────────────────────────────────────────────────
    if not args.task:
        parser.error("task is required in single mode (use --batch for batch mode)")

    task_dir = find_task_dir(args.task)
    if not task_dir:
        print(f"❌ Task not found: {args.task}", file=sys.stderr)
        print(f"\nAvailable tasks (first 20):")
        for i, d in enumerate(sorted(TASKS_DIR.iterdir())):
            if d.is_dir() and (d / "task.yaml").exists():
                print(f"  {d.name}")
                if i >= 19:
                    print(f"  ...")
                    break
        sys.exit(1)

    print(f"\nTask: {task_dir.name}")
    print(f"Path: {task_dir}")
    native_sandbox = "sandbox" if args.sandbox else "gateway"
    print(f"Mode: native={native_sandbox}, ce-runner=sandbox (always)")
    print(f"Run:  {args.mode}")

    results = {}
    start_time = time.time()

    if args.mode in ("native", "both"):
        trace = run_native(task_dir, config=args.config,
                          sandbox=args.sandbox, sandbox_image=args.sandbox_image)
        results["native"] = trace

    if args.mode in ("ce-runner", "both"):
        trace = run_ce_runner(task_dir, config=args.config,
                             sandbox=args.sandbox, sandbox_image=args.sandbox_image,
                             timeout=args.timeout)
        results["ce-runner"] = trace

    elapsed = time.time() - start_time

    # Summary
    print(f"\n{'='*60}")
    print(f"  Summary")
    print(f"{'='*60}")
    print(f"  Total time: {elapsed:.1f}s")

    for mode, trace_path in results.items():
        if trace_path:
            print(f"  [{mode:10s}] ✅ {trace_path}")
        else:
            print(f"  [{mode:10s}] ❌ No trace generated")

    if results.get("native") and results.get("ce-runner"):
        print(f"\n  You can now compare traces:")
        print(f"    python scripts/check_trace_timestamps.py {results['native']}")
        print(f"    python scripts/check_trace_timestamps.py {results['ce-runner']}")
        print(f"\n  Or run tests:")
        print(f"    pytest tests/test_trace_timestamps.py -v")


def _run_batch_mode(args) -> None:
    """Run batch comparison for multiple tasks."""
    task_ids = args.tasks or []
    has_filter = any([
        args.tasks_file, args.range_str, args.tag,
        args.prefix, args.filter_str,
    ])

    if not task_ids and not has_filter:
        print("❌ Batch mode requires --tasks, --range, --tag, --prefix, --filter, or --tasks-file",
              file=sys.stderr)
        sys.exit(1)

    print(f"\n{'='*60}")
    print(f"  Batch Comparison")
    print(f"{'='*60}")
    native_sandbox = "sandbox" if args.sandbox else "gateway"
    print(f"  Mode:  native={native_sandbox}, ce-runner=sandbox (always)")
    print(f"  Parallel: {args.parallel}")
    print(f"  Timeout: {args.timeout}s")
    if task_ids:
        print(f"  Tasks:  {' '.join(task_ids)}")

    start_time = time.time()
    results: dict[str, dict[str, Path | None]] = {}

    # Run native batch
    if args.mode in ("native", "both"):
        native_results = run_native_batch(
            task_ids=task_ids, config=args.config, sandbox=args.sandbox,
            sandbox_image=args.sandbox_image, timeout=args.timeout,
            parallel=args.parallel, tag=args.tag, range_str=args.range_str,
            prefix=args.prefix, filter_str=args.filter_str,
            tasks_file=args.tasks_file,
        )
        for tid, path in native_results.items():
            results.setdefault(tid, {})["native"] = path
        print(f"\n  [native-batch] {sum(1 for v in native_results.values() if v is not None)}/{len(native_results)} succeeded")

    # Run ce-runner batch
    if args.mode in ("ce-runner", "both"):
        ce_results = run_ce_runner_batch(
            task_ids=task_ids, config=args.config, sandbox=args.sandbox,
            sandbox_image=args.sandbox_image, timeout=args.timeout,
            parallel=args.parallel, tag=args.tag, range_str=args.range_str,
            prefix=args.prefix, filter_str=args.filter_str,
            tasks_file=args.tasks_file,
        )
        for tid, path in ce_results.items():
            results.setdefault(tid, {})["ce-runner"] = path
        print(f"\n  [ce-runner-batch] {sum(1 for v in ce_results.values() if v is not None)}/{len(ce_results)} succeeded")

    elapsed = time.time() - start_time

    # Summary
    print(f"\n{'='*60}")
    print(f"  Batch Summary — {len(results)} tasks, {elapsed:.1f}s")
    print(f"{'='*60}")

    print(f"\n  {'Task':<40s} {'Native':>10s} {'ce-runner':>10s}")
    print(f"  {'─'*40} {'─'*10} {'─'*10}")

    for tid in sorted(results.keys()):
        native_ok = "✅" if results[tid].get("native") else "❌"
        ce_ok = "✅" if results[tid].get("ce-runner") else "❌"
        print(f"  {tid:<40s} {native_ok:>10s} {ce_ok:>10s}")

    if results.get("native") and results.get("ce-runner"):
        print(f"\n  Compare traces:")
        print(f"    python scripts/check_trace_timestamps.py <trace_file>")
        print(f"    pytest tests/test_trace_timestamps.py -v")


if __name__ == "__main__":
    main()
