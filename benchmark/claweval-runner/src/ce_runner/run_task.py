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

"""Run claw-eval tasks using openclaw agent with full trace conversion and grading.

Usage:
  python3 run_task.py run <task_dir> [--timeout SECONDS] [--config CONFIG]
  python3 run_task.py batch --prefix T --range 1-5 --parallel 3 --config config.yaml

Supports T (general), M (multimodal), and C (user_agent/conversational) tasks.

Pipeline per task (always-sandbox mode):
  Phase 1: Execute - openclaw agent with Docker container + MCP sandbox bridge
  Phase 2: Convert - transform openclaw session JSONL to claw-eval trace JSONL
  Phase 3: Grade   - run claw-eval grader on the converted trace

C tasks (user_agent): Multi-round agent-UserAgent dialogue loop.
M tasks (multimodal): First turn via HTTP API for image attachments; sandbox
  container provides isolated filesystem + Bash/Read/Write tools.
"""

import argparse
import os
import re
import sys
import time
from pathlib import Path

from . import batch_runner
from .sandbox import (execute_task_sandbox, convert_and_grade_sandbox)
from .infra import check_gateway, cleanup_config, cleanup_mock_services

# Add claw-eval to path (repo root is two levels up from src/ce_runner/)
_CLAW_EVAL_SRC = Path(__file__).resolve().parent.parent.parent / "claw-eval" / "src"
if _CLAW_EVAL_SRC.is_dir():
    sys.path.insert(0, str(_CLAW_EVAL_SRC))

from ._common import (DEFAULT_AGENT_TIMEOUT_S, OPENCLAW_CONFIG, load_config,
                       load_task_yaml, log, make_trace_dir,
                       require_valid_config)

from . import __version__ as VERSION  # single source of truth

SCRIPT_DIR = str(Path(__file__).resolve().parent)



def get_judge_config(config: dict) -> dict:
    """Extract judge config from loaded yaml.

    Falls back to environment variables JUDGE_API_KEY, JUDGE_BASE_URL, JUDGE_MODEL_ID
    if not specified in config. Validation/abort on missing fields is delegated
    to :func:`require_valid_config` at the CLI entry points.
    """
    judge = config.get("judge", {})
    api_key = judge.get("api_key") or os.environ.get("JUDGE_API_KEY", "")
    base_url = judge.get("base_url") or os.environ.get("JUDGE_BASE_URL", "")
    model = judge.get("model_id") or os.environ.get("JUDGE_MODEL_ID", "")
    return {"api_key": api_key, "base_url": base_url, "model": model}


def get_model_config(config: dict) -> dict:
    """Extract model config from loaded yaml.

    Falls back to environment variables MODEL_API_KEY, MODEL_BASE_URL, MODEL_ID
    if not specified in config. Validation/abort on missing fields is delegated
    to :func:`require_valid_config` at the CLI entry points.
    """
    model = config.get("model", {})
    api_key = model.get("api_key") or os.environ.get("MODEL_API_KEY", "")
    base_url = model.get("base_url") or os.environ.get("MODEL_BASE_URL", "")
    model_id = model.get("model_id") or os.environ.get("MODEL_ID", "")
    return {"api_key": api_key, "base_url": base_url, "model_id": model_id}


def get_user_agent_config(config: dict) -> dict:
    """Extract user_agent_model config from loaded yaml.

    Falls back to judge config if user_agent_model is not specified.
    """
    ua = config.get("user_agent_model", {})
    judge = config.get("judge", {})
    api_key = ua.get("api_key") or judge.get("api_key") or os.environ.get("JUDGE_API_KEY", "")
    base_url = ua.get("base_url") or judge.get("base_url") or os.environ.get("JUDGE_BASE_URL", "")
    model_id = ua.get("model_id") or judge.get("model_id") or os.environ.get("JUDGE_MODEL_ID", "")
    return {"api_key": api_key, "base_url": base_url, "model_id": model_id}




def resolve_task(task_path: str) -> tuple[str, str]:
    """Resolve task directory or file to (task_yaml_abs, task_dir_abs)."""
    p = Path(task_path)
    if p.is_dir():
        task_yaml = p / "task.yaml"
    elif p.is_file():
        task_yaml = p
    else:
        log(f"Error: task path not found: {task_path}")
        sys.exit(1)
    if not task_yaml.exists():
        log(f"Error: task.yaml not found in {task_yaml.parent}")
        sys.exit(1)
    return str(task_yaml.resolve()), str(task_yaml.parent.resolve())


def discover_tasks(tasks_dir: str, tag: str = None, range_str: str = None,
                   filter_str: str = None, prefix: str = None) -> list[str]:
    """Discover and filter task directories (matches claw-eval logic)."""
    p = Path(tasks_dir)
    if not p.is_dir():
        log(f"Error: tasks directory not found: {tasks_dir}")
        sys.exit(1)

    # Step 1: discover all task dirs
    task_dirs = sorted(
        str(d) for d in p.iterdir()
        if d.is_dir() and (d / "task.yaml").exists()
    )

    # Step 2: --prefix exact prefix match on directory name
    if prefix:
        task_dirs = [d for d in task_dirs if Path(d).name.startswith(prefix)]

    # Step 3: --filter substring (case-insensitive)
    if filter_str:
        filt = filter_str.lower()
        task_dirs = [d for d in task_dirs if filt in d.lower()]

    # Step 4: --tag exact match on tags list
    if tag:
        filtered = []
        for d in task_dirs:
            td = load_task_yaml(os.path.join(d, "task.yaml"))
            if tag in td.get("tags", []):
                filtered.append(d)
        task_dirs = filtered

    # Step 5: --range numeric ID range (with optional step, e.g. '1-10:2')
    if range_str:
        m = re.match(r"(\d+)-(\d+)(?::(\d+))?$", range_str)
        if not m:
            log(f"[ERROR] Invalid --range format: {range_str} (expected L-R or L-R:step, e.g. 1-10 or 1-10:2)")
            sys.exit(1)
        lo, hi = int(m.group(1)), int(m.group(2))
        step = int(m.group(3)) if m.group(3) else 1
        if step < 1:
            log(f"[ERROR] Invalid --range step: {step} (must be >= 1)")
            sys.exit(1)
        # Sort by name for consistent ordering, then slice by 1-based positional index
        task_dirs = sorted(task_dirs, key=lambda d: Path(d).name)
        task_dirs = task_dirs[lo - 1 : hi : step]

    return task_dirs


# ── Single task runner ───────────────────────────────────────────────────────

def run_single(args):
    """Run a single task through the full pipeline."""
    task_path = args.task
    timeout = args.timeout
    config_path = getattr(args, "config", None)
    sandbox_image = getattr(args, "sandbox_image", None)

    if getattr(args, "sandbox", False):
        log("[WARNING] --sandbox is deprecated: always-sandbox mode is now the default")
    if getattr(args, "sandbox_tools", False):
        log("[WARNING] --sandbox-tools is deprecated: always-sandbox mode is now the default")

    cfg = load_config(config_path) if config_path else {}
    judge_config = get_judge_config(cfg)
    model_config = get_model_config(cfg)
    ua_config = get_user_agent_config(cfg)
    require_valid_config(config_path, judge_config, model_config)

    task_yaml, task_dir = resolve_task(task_path)
    task = load_task_yaml(task_yaml)
    task_id = task["task_id"]

    log("=" * 42)
    log(f" Task: {task_id}")
    log(f" YAML: {task_yaml}")
    log(f" Timeout: {timeout}s")
    log(f" Mode: sandbox")
    log("=" * 42)

    trace_dir = make_trace_dir(args.trace_prefix)

    wall_start = time.time()

    skip_cleanup_dirs = cfg.get("runner", {}).get("skip_cleanup_agent_dirs", False)

    gateway_port = check_gateway(OPENCLAW_CONFIG)
    if not gateway_port:
        log(f"[ERROR] openclaw gateway is not running")
        sys.exit(1)
    log(f"  Gateway: running on port {gateway_port}")

    # atexit emergency cleanup: if this process crashes (uncaught exception,
    # sys.exit, KeyboardInterrupt), kill residual mock services and remove
    # orphan agent/MCP config. Batch mode has its own atexit handler; this
    # one is only for single-task mode.
    import atexit
    _cleanup_done = {"done": False}

    def _emergency_cleanup():
        if _cleanup_done["done"]:
            return
        _cleanup_done["done"] = True
        try:
            cleanup_mock_services()
            log("[atexit-cleanup] cleanup_mock_services succeeded")
        except Exception as e:
            log(f"[atexit-cleanup] cleanup_mock_services failed: {e}")
        try:
            cleanup_config(skip_dirs=False)
            log("[atexit-cleanup] cleanup_config succeeded")
        except Exception as e:
            log(f"[atexit-cleanup] cleanup_config failed: {e}")

    atexit.register(_emergency_cleanup)

    log(f"\n[Phase 1] Executing openclaw agent via sandbox...")
    exec_result = execute_task_sandbox(
        task_yaml, task_dir, trace_dir,
        model_config=model_config,
        sandbox_image=sandbox_image,
        timeout=timeout,
        gateway_port=gateway_port,
        ua_config=ua_config,
    )
    if exec_result.get("error"):
        log(f"[ERROR] {exec_result['error']}")
        cleanup_config(skip_dirs=skip_cleanup_dirs)
        sys.exit(1)

    log(f"\n[Phase 2+3] Converting and grading...")
    result = convert_and_grade_sandbox(
        task_id, exec_result["session_file"], task_yaml, trace_dir, judge_config,
        env_snapshot_path=exec_result.get("env_snapshot_path"),
        session_id=exec_result.get("session_id", ""),
    )
    cleanup_config(skip_dirs=skip_cleanup_dirs)
    atexit.unregister(_emergency_cleanup)

    result["wall_time_s"] = round(time.time() - wall_start, 2)

    # ── Write session_map.json (trace ↔ openclaw session mapping) ───────────
    trace_path = result.get("trace_file")
    archive_path = result.get("session_archive_file")
    if trace_path or archive_path:
        import json as _json
        session_map_file = os.path.join(trace_dir, "session_map.json")
        with open(session_map_file, "w") as _f:
            _json.dump({
                "trace_dir": trace_dir,
                "sessions_dir": "sessions",
                "entries": [{
                    "task_id": task_id,
                    "trial": 1,
                    "session_id": exec_result.get("session_id", ""),
                    "trace_file": (os.path.relpath(trace_path, trace_dir)
                                   if trace_path else ""),
                    "session_file": (os.path.relpath(archive_path, trace_dir)
                                     if archive_path else ""),
                    "original_session_path": result.get("session_origin_file") or "",
                }],
            }, _f, indent=2, ensure_ascii=False)
        log(f" Session map: {session_map_file}")

    if result.get("error"):
        log(f"\n[ERROR] {result['error']}")
        sys.exit(1)

    status = "PASS" if result["passed"] else "FAIL"
    log(f"\n{'=' * 42}")
    log(f" Done: {task_id}")
    log(f" Score: {result['task_score']:.2f}  {status}")
    log(f"  completion:    {result['completion']:.2f}")
    log(f"  robustness:    {result['robustness']:.2f}")
    log(f"  communication: {result['communication']:.2f}")
    log(f"  safety:        {result['safety']:.2f}")
    log(f" Trace: {result.get('trace_file', 'N/A')}")
    log(f" Time: {result['wall_time_s']:.1f}s")
    log(f"{'=' * 42}")


# ── CLI ──────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description="Run claw-eval tasks using openclaw agent.",
    )
    parser.add_argument("-v", "--version", action="version", version=VERSION)

    subparsers = parser.add_subparsers(dest="command")

    # run subcommand
    p_run = subparsers.add_parser("run", help="Run a single task")
    p_run.add_argument("task", help="Task directory or task.yaml file path")
    p_run.add_argument(
        "--timeout", type=int, default=DEFAULT_AGENT_TIMEOUT_S,
        help=(f"Per-call agent timeout in seconds (default: {DEFAULT_AGENT_TIMEOUT_S}). "
              "For T/M tasks this is the total wall (single CLI subprocess or HTTP "
              "request). For C tasks each round gets its own budget, so the worst-case "
              "total wall is approximately this x max_rounds (configured in task.yaml)."))
    p_run.add_argument("--config", default=None, help="Path to config.yaml")
    p_run.add_argument("--sandbox", action="store_true", default=False,
                       help="(deprecated) Always-sandbox mode is now the default")
    p_run.add_argument("--sandbox-tools", action="store_true", default=False,
                       help="(deprecated) Always-sandbox mode is now the default")
    p_run.add_argument("--mcp-server", action="store_true", default=False,
                       help="(deprecated) MCP server mode is now the default")
    p_run.add_argument("--sandbox-image", default=None,
                       help="Docker image for sandbox (default: claw-eval-agent:latest)")
    p_run.add_argument("--trace-prefix", default="openclaw",
                       help="Prefix for trace directory name (default: openclaw)")

    # batch subcommand
    p_batch = subparsers.add_parser(
        "batch",
        help="Run multiple tasks in parallel",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""\
Task filtering examples:

  Select by tag (exact match on task YAML 'tags' field):
    --tag general          All T + C tasks (both have 'general' tag)
    --tag user_agent       Only C tasks (user_agent tag is unique to C)
    --tag multimodal       Only M tasks
    --tag multi_service    T tasks that use multiple services

  Select by numeric range (positional slice on sorted task list):
    --range 1-10           First 10 tasks (alphabetically sorted)
    --range 50-104         Tasks at positions 50-104
    --range 1-10:2         Every 2nd task within positions 1-10 (1,3,5,7,9)
    Note: --range slices by position, not by task numeric ID.
    Optional ':step' subsamples within the range (step defaults to 1).
    Use --prefix to scope to a specific prefix first.

  Combine tag + range:
    --tag general --range 1-10        First 10 tasks tagged 'general'
    --tag multi_service --range 1-50  First 50 multi-service tasks

  Filter by substring (case-insensitive, matches directory name):
    --filter email         Tasks with 'email' in the name
    --filter zh            Chinese-language tasks
    --filter score_canon   Specific task by name fragment
    Note: --filter is a substring match, NOT a prefix match.
    '--filter C' matches any task containing 'c' (case-insensitive).

  Select by prefix (exact prefix match on directory name):
    --prefix C                        All C tasks
    --prefix M                        All M tasks
    --prefix T                        All T tasks
    --prefix T --range 1-10           T001 to T010
    --prefix C --range 1-10           C01 to C10
    --prefix M --range 1-10           M001 to M010
    Note: --range slices by position after prefix filtering.
    Without --prefix, it slices the full sorted task list.
""",
    )
    p_batch.add_argument("--tasks-dir", default=None, help="Tasks directory (default: claw-eval/tasks)")
    p_batch.add_argument("--tasks-file", default=None,
                         help="File with one exact task name per line (e.g. T001zh_email_triage)")
    p_batch.add_argument("--tasks-string", default=None,
                         help="Comma-separated exact task names "
                              "(e.g. 'T091_pinbench_humanize_blog,T096_pinbench_business_metrics_summary'). "
                              "Mutually exclusive with --tasks-file/--prefix/--filter/--tag/--range")
    p_batch.add_argument("--prefix", default=None,
                         help="Only run tasks whose directory name starts with this prefix "
                              "(e.g. 'C', 'M', 'T'). Use with --range to slice a specific prefix")
    p_batch.add_argument("--filter", default=None,
                         help="Only run tasks matching this substring (e.g. 'en_' or 'T01')")
    p_batch.add_argument("--tag", default=None,
                         help="Only run tasks with this tag (e.g. 'multimodal', 'general')")
    p_batch.add_argument("--range", default=None,
                         help="Positional range with optional step (e.g. '1-10' or '1-10:2'). "
                              "Slices the sorted task list by position; step defaults to 1. "
                              "Use --prefix to scope to T/C/M tasks")
    p_batch.add_argument("--parallel", type=int, default=4, help="Number of parallel workers (default: 4)")
    p_batch.add_argument("--grade-parallel", type=int, default=0,
                         help="Parallel workers for grading (judge API). "
                              "Default: min(parallel, 2) to avoid LLM judge rate-limits")
    p_batch.add_argument("--chunk-size", type=int, default=4,
                         help="Number of tasks to setup/execute/cleanup per chunk. "
                              "Controls peak memory usage by limiting concurrent MCP/mock "
                              "processes. Auto-raised to --parallel if smaller (default: 4)")
    p_batch.add_argument("--config", default=None, help="Path to config.yaml")
    p_batch.add_argument(
        "--timeout", type=int, default=DEFAULT_AGENT_TIMEOUT_S,
        help=(f"Per-call agent timeout in seconds (default: {DEFAULT_AGENT_TIMEOUT_S}). "
              "For T/M tasks this is the total wall (single CLI subprocess or HTTP "
              "request). For C tasks each round gets its own budget, so the worst-case "
              "total wall is approximately this x max_rounds (configured in task.yaml)."))
    p_batch.add_argument("--trials", type=int, default=1, help="Number of trials per task (default: 1)")
    p_batch.add_argument("--sandbox-image", default=None,
                         help="Docker image for sandbox (default: claw-eval-agent:latest)")
    p_batch.add_argument("--sandbox", action="store_true", default=False,
                         help="(deprecated) Always-sandbox mode is now the default")
    p_batch.add_argument("--sandbox-tools", action="store_true", default=False,
                         help="(deprecated) Always-sandbox mode is now the default")
    p_batch.add_argument("--mcp-server", action="store_true", default=False,
                         help="(deprecated) MCP server mode is now the default")
    p_batch.add_argument("--trace-prefix", default="openclaw",
                         help="Prefix for trace directory name (default: openclaw)")
    p_batch.add_argument("--skip-preflight", action="store_true", default=False,
                         help="Skip openclaw plugins + docker pre-flight checks")

    args = parser.parse_args()

    if args.command == "batch":
        batch_runner.run_batch(
            args,
            get_judge_config=get_judge_config,
            get_model_config=get_model_config,
            get_user_agent_config=get_user_agent_config,
            discover_tasks=discover_tasks,
        )
    elif args.command == "run":
        run_single(args)
    else:
        # No subcommand: backward compat — re-parse as single task
        parser2 = argparse.ArgumentParser()
        parser2.add_argument("-v", "--version", action="version", version=VERSION)
        parser2.add_argument("task")
        parser2.add_argument("--timeout", type=int, default=DEFAULT_AGENT_TIMEOUT_S)
        parser2.add_argument("--config", default=None)
        parser2.add_argument("--sandbox-image", default=None)
        parser2.add_argument("--sandbox", action="store_true", default=False,
                            help="(deprecated) Always-sandbox mode is now the default")
        parser2.add_argument("--sandbox-tools", action="store_true", default=False,
                            help="(deprecated) Always-sandbox mode is now the default")
        parser2.add_argument("--mcp-server", action="store_true", default=False,
                            help="(deprecated) MCP server mode is now the default")
        parser2.add_argument("--trace-prefix", default="openclaw")
        args2 = parser2.parse_args()
        args2.command = "run"
        run_single(args2)


if __name__ == "__main__":
    main()
