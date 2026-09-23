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

"""End-to-end integration test: batch-run tasks -> compare results -> run full pytest suite.

Examples:
    # Default: run first 5 T-tasks (native + ce-runner) + full pytest
    python scripts/run_integration_test.py

    # Specify task type and count
    python scripts/run_integration_test.py --type T --count 3
    python scripts/run_integration_test.py --type M --count 2

    # Select tasks by position range (1-based, inclusive)
    python scripts/run_integration_test.py --type T --range 151-165
    python scripts/run_integration_test.py --type C --range 1-10

    # Range with step (ce-runner mode only)
    python scripts/run_integration_test.py --range 1-100:3 --mode ce-runner

    # All tasks: --range alone selects all types (T+M+C)
    python scripts/run_integration_test.py --range 1-303

    # Explicitly specify ALL types
    python scripts/run_integration_test.py --type ALL --count 0

    # Run ce-runner only (skip native claw-eval)
    python scripts/run_integration_test.py --mode ce-runner

    # Run pytest only (skip task execution)
    python scripts/run_integration_test.py --pytest-only

    # Custom parallelism and timeout
    python scripts/run_integration_test.py --parallel 8 --timeout 900

    # Skip old trace cleanup
    python scripts/run_integration_test.py --no-cleanup
"""

import argparse
import os
import re
import sys
import time
import subprocess
import tempfile
import shutil
from pathlib import Path

# Allow importing from repo root (for tests/) and scripts/ (for run_task_compare)
_SCRIPT_DIR = Path(__file__).resolve().parent
_REPO_ROOT = _SCRIPT_DIR.parent
sys.path.insert(0, str(_REPO_ROOT))
sys.path.insert(0, str(_SCRIPT_DIR))

from run_task_compare import (
    run_native_batch, run_ce_runner_batch, CLAW_EVAL_DIR,
)
from tests.helpers import TRACES_DIR
from ce_runner._common import DEFAULT_AGENT_TIMEOUT_S as DEFAULT_TIMEOUT

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

DEFAULT_TYPE = "T"
DEFAULT_COUNT = 5
DEFAULT_PARALLEL = 4
DEFAULT_TRIALS = 1


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
def _discover_tasks(task_type: str | None) -> list[str]:
    """Discover all tasks of the given type from the filesystem.

    Scans claw-eval/tasks/ for directories starting with task_type that
    contain a task.yaml. Returns all tasks when task_type is None.

    Args:
        task_type: Task type prefix (T/M/C), None for all types.

    Returns:
        Sorted list of task IDs.
    """
    tasks_base = CLAW_EVAL_DIR / "tasks"
    if not tasks_base.exists():
        print(f"❌ 任务目录不存在: {tasks_base}", file=sys.stderr)
        sys.exit(1)

    all_tasks = sorted(
        d.name for d in tasks_base.iterdir()
        if d.is_dir() and (task_type is None or d.name.startswith(task_type)) and (d / "task.yaml").exists()
    )

    type_label = task_type or "ALL"
    if not all_tasks:
        print(f"❌ 未找到 {type_label} 类型任务", file=sys.stderr)
        sys.exit(1)

    return all_tasks


def _select_tasks(task_type: str | None, count: int, range_str: str | None = None, mode: str = "both") -> list[str]:
    """Select tasks of the specified type.

    Priority: range_str > count.

    Args:
        task_type: Task type (T/M/C), None for all types.
        count: Number of tasks, 0 for all.
        range_str: Position range, format ``L-R`` (1-based, inclusive) or
                   ``L-R:S`` (with step, ce-runner mode only),
                   e.g. ``"151-165"`` or ``"1-100:3"``.
        mode: Run mode (native/ce-runner/both).

    Returns:
        List of task IDs.
    """
    all_tasks = _discover_tasks(task_type)
    type_label = task_type or "ALL"

    if range_str:
        m = re.match(r"^(\d+)-(\d+)(?::(\d+))?$", range_str)
        if not m:
            print(f"❌ --range 格式错误: {range_str}（期望 L-R 或 L-R:S，如 1-10 或 1-100:3）", file=sys.stderr)
            sys.exit(1)
        lo, hi = int(m.group(1)), int(m.group(2))
        step = int(m.group(3)) if m.group(3) else 1
        if step > 1 and mode != "ce-runner":
            print(f"❌ --range 步长模式 (L-R:S) 仅在 --mode ce-runner 下支持", file=sys.stderr)
            sys.exit(1)
        if lo < 1 or hi > len(all_tasks) or lo > hi or step < 1:
            print(f"❌ --range {range_str} 越界或参数无效（共 {len(all_tasks)} 个 {type_label} 类型任务）",
                  file=sys.stderr)
            sys.exit(1)
        selected = all_tasks[lo - 1 : hi : step]
        label = f"range {range_str}"
    elif count <= 0:
        selected = all_tasks
        label = "全部"
    else:
        selected = all_tasks[:count]
        label = f"前 {count} 个"

    print(f"📋 从 {len(all_tasks)} 个 {type_label} 类型任务中选择 {len(selected)} 个 ({label}):")
    for i, tid in enumerate(selected, 1):
        print(f"   {i}. {tid}")
    return selected


def _create_temp_tasks_dir(task_ids: list[str]) -> str:
    """Create a temp directory mirroring the claw-eval layout with only the selected tasks.

    Structure:
      /tmp/ce-integ-xxx/
        mock_services/   → symlink to claw-eval/mock_services/
        config/          → symlink to claw-eval/config/
        tasks/           → real dir, containing symlinks to selected task dirs

    This preserves the relative path resolution that claw-eval's ServiceManager
    depends on (cwd = tasks_dir.parent, so mock_services/ must be a sibling of tasks/).

    Returns the path to the tasks/ subdirectory (to be used as --tasks-dir).
    """
    tmpdir = tempfile.mkdtemp(prefix="ce-integ-")
    base = Path(tmpdir)

    # Symlink sibling directories that claw-eval resolves relative to tasks/../
    for sibling in ("mock_services", "config"):
        src = CLAW_EVAL_DIR / sibling
        if src.exists():
            (base / sibling).symlink_to(src)

    # Create tasks/ subdirectory with symlinks to selected tasks only
    tasks_sub = base / "tasks"
    tasks_sub.mkdir()
    tasks_base = CLAW_EVAL_DIR / "tasks"
    for tid in task_ids:
        src = tasks_base / tid
        dst = tasks_sub / tid
        if src.exists():
            dst.symlink_to(src)
        else:
            print(f"  \u26a0\ufe0f  \u4efb\u52a1\u76ee\u5f55\u4e0d\u5b58\u5728: {src}", file=sys.stderr)

    return str(tasks_sub)


def _create_temp_tasks_file(task_ids: list[str]) -> str:
    """Create a temp file listing task IDs, one per line."""
    fd, path = tempfile.mkstemp(prefix="ce-integ-", suffix=".txt")
    with os.fdopen(fd, 'w') as f:
        for tid in task_ids:
            f.write(tid + '\n')
    return path


def _cleanup_old_traces(keep_latest: int = 1):
    """Remove old trace directories, keeping only the latest N."""
    if not TRACES_DIR.exists():
        return

    dirs = sorted(
        [d for d in TRACES_DIR.iterdir() if d.is_dir()],
        key=lambda d: d.stat().st_mtime,
        reverse=True
    )

    if len(dirs) <= keep_latest:
        print(f"ℹ️  已有 {len(dirs)} 个trace目录，无需清理")
        return

    to_remove = dirs[keep_latest:]
    print(f"🧹 清理 {len(to_remove)} 个旧trace目录...")
    for d in to_remove:
        print(f"   删除: {d.name}")
        import shutil
        shutil.rmtree(d, ignore_errors=True)

    print(f"✅ 保留最新 {keep_latest} 个目录")


def _run_pytest(verbose: bool = False, report_file: str | None = None) -> bool:
    """Run the full pytest test suite.

    Args:
        verbose: Whether to show verbose output.
        report_file: Path to the report file.

    Returns:
        True if all tests passed.
    """
    print(f"\n{'='*72}")
    print("  Phase 3: 全量 pytest 测试")
    print(f"{'='*72}")

    cmd = [
        sys.executable, "-m", "pytest",
        "tests/",
        "-v" if verbose else "-q",
        "--tb=short" if verbose else "--tb=no",
        "-rs",  # show skip reasons
    ]

    if report_file:
        cmd.extend(["--json-report", f"--json-report-file={report_file}"])

    print(f"\n🔍 运行: {' '.join(cmd)}")
    print(f"📂 工作目录: {_REPO_ROOT}")

    result = subprocess.run(
        cmd,
        cwd=str(_REPO_ROOT),
        timeout=300,  # 5-minute timeout
    )

    return result.returncode == 0


def _print_summary(phase1_ok: bool, phase2_ok: bool, phase3_ok: bool, elapsed: float):
    """Print final summary."""
    print(f"\n{'='*72}")
    print("  📊 集成测试总结")
    print(f"{'='*72}")
    print(f"  Phase 1 (批量执行):   {'✅ 通过' if phase1_ok else '❌ 失败'}")
    print(f"  Phase 2 (产物检查):   {'✅ 通过' if phase2_ok else '❌ 失败'}")
    print(f"  Phase 3 (全量测试):   {'✅ 通过' if phase3_ok else '❌ 失败'}")
    print(f"  总耗时:               {elapsed:.1f}s")
    print(f"{'='*72}")

    all_ok = phase1_ok and phase2_ok and phase3_ok
    if all_ok:
        print("\n🎉 全部通过！")
    else:
        print("\n❌ 存在失败项，请检查上方输出")

    return all_ok



# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
def main():
    parser = argparse.ArgumentParser(
        description="一键完整集成测试：批量运行任务 → 对比结果 → 执行全量pytest测试",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
使用示例:
  # 默认：运行前5个T任务 + 全量pytest
  %(prog)s

  # 运行3个T任务（不运行pytest）
  %(prog)s --type T --count 3 --no-pytest

  # 按位置范围选取 T 任务 151~165
  %(prog)s --type T --range 151-165 --mode ce-runner

  # 直接指定一或多个 task_id（回归/单 task 跳跑）
  %(prog)s --tasks-string T091_pinbench_humanize_blog --mode ce-runner --no-pytest
  %(prog)s --tasks-string T003zh,T004 --trials 2 --no-pytest

  # 运行2个M任务 + ce-runner only
  %(prog)s --type M --count 2 --mode ce-runner

  # 运行前 3 个 C 任务 + 详细输出
  %(prog)s --type C --count 3 --verbose

  # 仅执行pytest（跳过任务运行）
  %(prog)s --pytest-only

  # 清理旧trace + 并行8个
  %(prog)s --cleanup --parallel 8
        """,
    )

    # Task selection
    parser.add_argument("--type", choices=["T", "M", "C", "ALL"], default=DEFAULT_TYPE,
                        help=f"任务类型，ALL 表示全部类型 (默认: {DEFAULT_TYPE})")
    parser.add_argument("--count", type=int, default=DEFAULT_COUNT,
                        help=f"该类型的任务数量（0=全部，默认: {DEFAULT_COUNT}）。与 --range 互斥")
    parser.add_argument("--range", dest="range_str", default=None,
                        help="按位置范围选取任务，格式 L-R（1-based 闭区间）或 L-R:S（带步长），"
                             "如 '151-165' 或 '1-100:3'。步长模式仅 --mode ce-runner 下支持。"
                             "覆盖 --count。"
                             "当单独使用 --range 而不指定 --type 时自动选取全部类型")
    parser.add_argument("--tasks-string", dest="tasks_string", default=None,
                        help="按 task_id 直接指定一个或多个任务，逗号分隔，"
                             "如 'T091_pinbench_humanize_blog' 或 'T003zh,T004'。"
                             "与 --type/--count/--range 互斥；用于回归或单 task 跳跑")

    # Execution mode
    parser.add_argument("--mode", choices=["native", "ce-runner", "both"],
                        default="both",
                        help="运行模式 (默认: both)")
    parser.add_argument("--parallel", type=int, default=DEFAULT_PARALLEL,
                        help=f"并行数 (默认: {DEFAULT_PARALLEL})")
    parser.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT,
                        help=f"单任务超时秒数 (默认: {DEFAULT_TIMEOUT})")
    parser.add_argument("--trials", type=int, default=DEFAULT_TRIALS,
                        help=f"每任务重复次数 (默认: {DEFAULT_TRIALS}，同 task 内 trials 串行执行)")
    parser.add_argument("--sandbox", action="store_true", default=True,
                        help="使用沙盒模式 (默认)")
    parser.add_argument("--config", default="claw-eval/config.yaml",
                        help="配置文件路径 (默认: claw-eval/config.yaml)")

    # Advanced options
    parser.add_argument("--cleanup", action="store_true",
                        help="运行前清理旧trace目录")
    parser.add_argument("--no-cleanup", action="store_false", dest="cleanup",
                        help="不清理旧trace (默认)")
    parser.add_argument("--keep-traces", type=int, default=1,
                        help="清理时保留最新的N个trace目录 (默认: 1)")
    parser.add_argument("--no-pytest", action="store_true",
                        help="跳过pytest测试")
    parser.add_argument("--pytest-only", action="store_true",
                        help="仅执行pytest，跳过任务运行")
    parser.add_argument("--verbose", action="store_true",
                        help="pytest详细输出")
    parser.add_argument("--report", type=str, default=None,
                        help="pytest报告文件路径 (JSON格式)")

    args = parser.parse_args()

    # When --range is used alone without --type explicitly set, auto-switch to all-types mode
    # argparse has no built-in way to detect "was this explicitly set", so check sys.argv
    type_explicitly_set = any(a.startswith("--type") for a in sys.argv[1:])
    if args.range_str and not type_explicitly_set:
        args.type = "ALL"

    task_type = None if args.type == "ALL" else args.type

    total_start = time.time()

    # =====================================================================
    # Phase 0: Preparation
    # =====================================================================
    print(f"\n{'='*72}")
    print("  🚀 一键完整集成测试")
    print(f"{'='*72}")

    # Select tasks: --tasks-string takes priority, mutually exclusive with --type/--count/--range
    if args.tasks_string:
        type_explicitly = any(a.startswith("--type") for a in sys.argv[1:])
        count_explicitly = any(a.startswith("--count") for a in sys.argv[1:])
        if type_explicitly or count_explicitly or args.range_str:
            print("❌ --tasks-string 与 --type/--count/--range 互斥", file=sys.stderr)
            sys.exit(1)
        requested = [n.strip() for n in args.tasks_string.split(",") if n.strip()]
        if not requested:
            print("❌ --tasks-string 为空", file=sys.stderr)
            sys.exit(1)
        all_tasks = set(_discover_tasks(None))
        missing = [t for t in requested if t not in all_tasks]
        if missing:
            print(f"❌ --tasks-string 包含未知任务: {', '.join(missing)}", file=sys.stderr)
            sys.exit(1)
        task_ids = requested
        print(f"📋 按 --tasks-string 选择 {len(task_ids)} 个任务:")
        for i, tid in enumerate(task_ids, 1):
            print(f"   {i}. {tid}")
    else:
        task_ids = _select_tasks(task_type, args.count, range_str=args.range_str, mode=args.mode)
    if not task_ids:
        print("❌ 未选择任何任务", file=sys.stderr)
        sys.exit(1)

    # Cleanup old traces
    if args.cleanup:
        _cleanup_old_traces(keep_latest=args.keep_traces)

    # =====================================================================
    # Phase 1: Batch task execution
    # =====================================================================
    phase1_ok = True

    if not args.pytest_only:
        print(f"\n{'='*72}")
        print(f"  Phase 1: 批量执行任务 ({len(task_ids)} 个)")
        print(f"{'='*72}")

        run_native_mode = args.mode in ("native", "both")
        run_ce_mode = args.mode in ("ce-runner", "both")

        trace_results = {}  # task_id -> {mode: trace_path}

        # --- Native claw-eval ---
        if run_native_mode:
            print(f"\n📦 运行原生 claw-eval (并行={args.parallel})...")
            tmp_tasks_dir = _create_temp_tasks_dir(task_ids)
            t0 = time.time()
            try:
                native_map = run_native_batch(
                    task_ids=task_ids,
                    config=args.config,
                    sandbox=args.sandbox,
                    timeout=args.timeout,
                    parallel=args.parallel,
                    trials=args.trials,
                    tasks_dir=tmp_tasks_dir,
                )
            finally:
                # tmp_tasks_dir is .../tasks/; remove the parent temp root
                shutil.rmtree(Path(tmp_tasks_dir).parent, ignore_errors=True)
            native_elapsed = time.time() - t0

            for tid, path in native_map.items():
                trace_results.setdefault(tid, {})["native"] = path
                status = "✅" if path else "❌"
                print(f"   {status} {tid}: {path.name if path else '失败'}")

            print(f"⏱️  原生耗时: {native_elapsed:.1f}s")

            # Check for failures
            native_failures = [tid for tid, path in native_map.items() if path is None]
            if native_failures:
                print(f"⚠️  原生失败: {', '.join(native_failures)}")
                phase1_ok = False

        # --- ce-runner ---
        if run_ce_mode:
            print(f"\n🏃 运行 ce-runner (并行={args.parallel})...")
            tmp_tasks_file = _create_temp_tasks_file(task_ids)
            t0 = time.time()
            try:
                ce_map = run_ce_runner_batch(
                    task_ids=task_ids,
                    config=args.config,
                    sandbox=args.sandbox,
                    timeout=args.timeout,
                    parallel=args.parallel,
                    trials=args.trials,
                    tasks_file=tmp_tasks_file,
                )
            finally:
                os.unlink(tmp_tasks_file)
            ce_elapsed = time.time() - t0

            for tid, path in ce_map.items():
                trace_results.setdefault(tid, {})["ce-runner"] = path
                status = "✅" if path else "❌"
                print(f"   {status} {tid}: {path.name if path else '失败'}")

            print(f"⏱️  ce-runner耗时: {ce_elapsed:.1f}s")

            # Check for failures
            ce_failures = [tid for tid, path in ce_map.items() if path is None]
            if ce_failures:
                print(f"⚠️  ce-runner失败: {', '.join(ce_failures)}")
                phase1_ok = False

        # --- Comparison summary ---
        print(f"\n{'─'*72}")
        print("  执行对比总结")
        print(f"{'─'*72}")
        print(f"  {'任务':<40s} {'原生':<10s} {'ce-runner':<12s}")
        print(f"  {'─'*40} {'─'*10} {'─'*12}")

        for tid in task_ids:
            native_ok = "✅" if trace_results.get(tid, {}).get("native") else "❌"
            ce_ok = "✅" if trace_results.get(tid, {}).get("ce-runner") else "❌"
            native_label = native_ok if run_native_mode else "-"
            ce_label = ce_ok if run_ce_mode else "-"
            print(f"  {tid:<40s} {native_label:<10s} {ce_label:<12s}")
    else:
        print("\n⏭️  跳过任务执行 (--pytest-only)")
        trace_results = {}

    # =====================================================================
    # Phase 2: Output artifact checks
    # =====================================================================
    phase2_ok = True

    if not args.pytest_only and trace_results:
        print(f"\n{'='*72}")
        print("  Phase 2: 产物格式检查")
        print(f"{'='*72}")

        # Check batch output files
        latest_trace_dir = None
        if TRACES_DIR.exists():
            dirs = sorted(
                [d for d in TRACES_DIR.iterdir() if d.is_dir() and d.name.startswith("openclaw")],
                key=lambda d: d.stat().st_mtime,
                reverse=True
            )
            if dirs:
                latest_trace_dir = dirs[0]

        if latest_trace_dir:
            batch_results = latest_trace_dir / "batch_results.json"
            batch_summary = latest_trace_dir / "batch_summary.json"

            print(f"\n📁 最新trace目录: {latest_trace_dir.name}")
            print(f"   batch_results.json: {'✅ 存在' if batch_results.exists() else '❌ 缺失'}")
            print(f"   batch_summary.json: {'✅ 存在' if batch_summary.exists() else '❌ 缺失'}")

            # Basic JSON format validation
            if batch_results.exists():
                try:
                    import json
                    with open(batch_results) as f:
                        data = json.load(f)
                    if isinstance(data, list):
                        # Only validate that expected task_ids are present in batch_results
                        # and their trial count == args.trials.
                        # Extra tasks hit by --filter substring matching are not checked.
                        print(f"   ✅ batch_results.json 格式正确 (list, 共 {len(data)} 个 task)")
                        by_id = {e.get("task_id"): e for e in data}
                        missing = [tid for tid in task_ids if tid not in by_id]
                        if missing:
                            print(f"   ❌ 预期 task 未出现在 batch_results: {missing}")
                            phase2_ok = False
                        else:
                            mismatched = []
                            for tid in task_ids:
                                trials_list = by_id[tid].get("trials", [])
                                if len(trials_list) != args.trials:
                                    mismatched.append(
                                        f"{tid}({len(trials_list)}/{args.trials})"
                                    )
                            if mismatched:
                                print(f"   ❌ trials 数不匹配: {', '.join(mismatched)}")
                                phase2_ok = False
                            else:
                                print(f"   ✅ 预期的 {len(task_ids)} 个 task 均输出且 trials={args.trials}")
                    else:
                        print(f"   ⚠️  batch_results.json 顶层不是list (可能是旧格式)")
                except Exception as e:
                    print(f"   ❌ batch_results.json 解析失败: {e}")
                    phase2_ok = False

            if batch_summary.exists():
                try:
                    import json
                    with open(batch_summary) as f:
                        data = json.load(f)
                    hat_keys = [k for k in data if k.startswith("pass_hat_")]
                    if hat_keys:
                        print(f"   ✅ batch_summary.json 包含 {', '.join(hat_keys)}")
                    else:
                        print(f"   ⚠️  batch_summary.json 缺少 pass_hat_* 字段")
                        phase2_ok = False
                except Exception as e:
                    print(f"   ❌ batch_summary.json 解析失败: {e}")
                    phase2_ok = False
        else:
            print("\n⚠️  未找到trace目录")
            phase2_ok = False
    else:
        print("\n⏭️  跳过产物检查 (--pytest-only 或无trace)")

    # =====================================================================
    # Phase 3: Full pytest suite
    # =====================================================================
    if args.no_pytest:
        print("\n⏭️  跳过pytest测试 (--no-pytest)")
        phase3_ok = True
    else:
        # Give gateway time to stabilise after batch cleanup restart
        STABILISE_WAIT = 10
        print(f"\n⏳ 等待 {STABILISE_WAIT}s 让 gateway 稳定...")
        time.sleep(STABILISE_WAIT)
        phase3_ok = _run_pytest(
            verbose=args.verbose,
            report_file=args.report,
        )

    # =====================================================================
    # Final summary
    # =====================================================================
    total_elapsed = time.time() - total_start
    all_ok = _print_summary(phase1_ok, phase2_ok, phase3_ok, total_elapsed)

    sys.exit(0 if all_ok else 1)


if __name__ == "__main__":
    main()
