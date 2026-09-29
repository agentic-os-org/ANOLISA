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

"""Batch execution: run multiple tasks in parallel with agent isolation."""

from __future__ import annotations

import atexit
import base64
import json
import math
import os
import shutil
import subprocess
import sys
import threading
import time
import traceback
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

from ._common import (OPENCLAW_CONFIG, _REPO_DIR, attach_log_file,
                       detach_log_file, is_sandbox_task, load_config,
                       load_task_yaml, log, make_trace_dir,
                       require_valid_config)
from .agent import last_agent_error, run_agent, run_agent_with_user_agent
from .infra import (check_gateway, cleanup_mock_services,
                    ensure_user_session_persistent, kill_mcp_bridges,
                    reap_orphan_agent_processes, restart_gateway,
                    stop_gateway)
from .parallel import (PORT_STRIDE, cleanup_parallel_workers,
                       reset_services_with_offset,
                       setup_parallel_workers, start_mock_services_with_offset)
from .preflight import find_missing_fixtures, run_preflight_checks
from .sandbox import (collect_env_snapshot, convert_and_grade_sandbox,
                      save_env_snapshot)
from .session_trace_converter import fetch_audit_data


def pass_at_k(n: int, c: int, k: int) -> float:
    """Unbiased pass@k estimator."""
    if n - c < k:
        return 1.0
    return 1.0 - math.comb(n - c, k) / math.comb(n, k)


def run_batch(args, get_judge_config, get_model_config, get_user_agent_config,
              discover_tasks):
    """Execute batch of tasks with chunked parallel agent execution.

    Architecture (always-sandbox, chunked):
    Tasks are split into chunks of --chunk-size. For each chunk:
      1. Setup: register per-task MCP servers + start mock services + restart gateway
      2. Execute: run agents in parallel via ThreadPoolExecutor (max_workers=parallel)
      3. Grade: convert + grade runs in a separate thread pool
      4. Cleanup: release MCP servers + kill mock services

    This ensures memory usage stays bounded regardless of total task count.

    The config extraction callables and discover_tasks are passed in
    from run_task to avoid circular imports.
    """
    tasks_dir = args.tasks_dir or os.path.join(_REPO_DIR, "claw-eval", "tasks")
    tag = args.tag
    range_str = getattr(args, "range", None)
    filter_str = getattr(args, "filter", None)
    prefix = getattr(args, "prefix", None)
    parallel = args.parallel
    config_path = args.config
    timeout = args.timeout
    trials = args.trials
    sandbox_image = getattr(args, "sandbox_image", None)
    chunk_size = getattr(args, "chunk_size", None)
    if chunk_size is None:
        chunk_size = parallel

    # Ensure chunk_size >= parallel (otherwise under-utilizes workers)
    if chunk_size < parallel:
        log(f"[WARNING] --chunk-size ({chunk_size}) < --parallel ({parallel}). "
            f"Adjusting chunk_size to {parallel}.")
        chunk_size = parallel

    # Load config
    cfg = load_config(config_path) if config_path else {}
    judge_config = get_judge_config(cfg)
    model_config = get_model_config(cfg)
    ua_config = get_user_agent_config(cfg)
    require_valid_config(config_path, judge_config, model_config)

    # Discover tasks
    tasks_file = getattr(args, "tasks_file", None)
    tasks_string = getattr(args, "tasks_string", None)
    has_filters = any(v is not None for v in (tag, range_str, filter_str, prefix))
    selectors_used = sum(bool(x) for x in (tasks_file, tasks_string, has_filters))
    if selectors_used > 1:
        log("[ERROR] --tasks-file, --tasks-string and filter options "
            "(--prefix/--filter/--tag/--range) are mutually exclusive.")
        sys.exit(1)

    if tasks_file:
        # Read exact task names from file (one per line)
        if not os.path.exists(tasks_file):
            log(f"[ERROR] Tasks file not found: {tasks_file}")
            sys.exit(1)
        with open(tasks_file) as f:
            task_names = [line.strip() for line in f if line.strip()]
        source_desc = f"file {tasks_file}"
    elif tasks_string:
        task_names = [n.strip() for n in tasks_string.split(",") if n.strip()]
        if not task_names:
            log("[ERROR] --tasks-string is empty.")
            sys.exit(1)
        source_desc = "--tasks-string"
    else:
        task_names = None
        source_desc = None

    if task_names is not None:
        task_dirs = []
        for name in task_names:
            task_path = os.path.join(tasks_dir, name)
            if os.path.isdir(task_path) and os.path.isfile(os.path.join(task_path, "task.yaml")):
                task_dirs.append(task_path)
            else:
                log(f"[WARNING] Task not found: {name}")
        if not task_dirs:
            log(f"No valid tasks found from {source_desc}.")
            sys.exit(1)
        log(f"Loaded {len(task_dirs)} tasks from {source_desc}")
    else:
        task_dirs = discover_tasks(tasks_dir, tag=tag, range_str=range_str,
                                   filter_str=filter_str, prefix=prefix)
        if not task_dirs:
            log("No tasks matched the given filters.")
            sys.exit(1)

    # Check gateway
    gateway_port = check_gateway(OPENCLAW_CONFIG)
    if not gateway_port:
        log("[ERROR] openclaw gateway is not running")
        sys.exit(1)

    # Build unique task list + task queue
    task_yamls_unique = []
    task_yaml_map = {}  # task_yaml -> task_dir
    task_meta = {}      # task_id -> {"task_name": ..., "difficulty": ...}
    for td in task_dirs:
        tyaml = os.path.join(td, "task.yaml")
        task_yamls_unique.append(tyaml)
        task_yaml_map[tyaml] = td
        try:
            ty = load_task_yaml(tyaml)
            task_meta[ty.get("task_id", os.path.basename(td))] = {
                "task_name": ty.get("task_name", "") or "",
                "difficulty": ty.get("difficulty", "") or "",
            }
        except Exception as e:
            log(f"[WARNING] Failed to read meta from {tyaml}: {e}")

    total_runs = len(task_yamls_unique) * trials
    trace_dir = make_trace_dir(getattr(args, 'trace_prefix', 'openclaw'))

    # Mirror every log() call to <trace_dir>/batch.log for postmortem debug.
    batch_log_path = os.path.join(trace_dir, "batch.log")
    attach_log_file(batch_log_path)
    log(f"Batch log: {batch_log_path}")

    n_with_files = sum(1 for td in task_dirs if is_sandbox_task(os.path.join(td, "task.yaml")))
    n_empty = len(task_dirs) - n_with_files

    # Split tasks into chunks
    n_chunks = math.ceil(len(task_yamls_unique) / chunk_size)

    log(f"Discovered {len(task_dirs)} tasks ({n_with_files} with sandbox_files, {n_empty} without)")
    log(f"Gateway running on port {gateway_port}")
    log(f"Model: {model_config['model_id']} @ {model_config['base_url']}")
    log(f"Judge: {judge_config['model']} @ {judge_config['base_url']}")
    log(f"Total task runs: {total_runs} ({len(task_dirs)} tasks x {trials} trials)")
    log(f"Parallel workers: {parallel}")
    log(f"Chunk size: {chunk_size} ({n_chunks} chunks)")
    log(f"Trace dir: {trace_dir}")
    log(f"Mode: always-sandbox (Docker)")
    log("")

    # ── Pre-flight: ensure systemd user session persists ──────────────────
    ensure_user_session_persistent()

    # ── Pre-flight: verify openclaw plugins + docker daemon are healthy ───
    # An intermittently broken openclaw install (e.g. a plugin that fails to
    # load) or an unreachable Docker daemon makes every task fail in confusing
    # ways, so abort the whole batch up front with an actionable message.
    if not getattr(args, "skip_preflight", False):
        ok, preflight_errors = run_preflight_checks()
        if not ok:
            log(f"[ERROR] Pre-flight environment check failed "
                f"({len(preflight_errors)} issue(s)):")
            for err in preflight_errors:
                log(f"  ✗ {err}")
            log("Fix the openclaw install (e.g. `npm i -g openclaw`) and the "
                "Docker daemon, then re-run. Use --skip-preflight to bypass.")
            sys.exit(2)
        log("Pre-flight environment check passed (openclaw plugins + docker)")
        log("")

    # ── Pre-flight: warn on missing env vars for mock services ────────────
    _web_real_warned_tasks: set[str] = set()
    for tyaml in task_yamls_unique:
        try:
            ty = load_task_yaml(tyaml)
            for svc in ty.get("services", []):
                if svc.get("name") in ("web_real", "web_real_injection"):
                    _web_real_warned_tasks.add(ty.get("task_id", ""))
                    break
        except Exception:
            pass
    if _web_real_warned_tasks:
        serp_key = os.environ.get("SERP_DEV_KEY", "")
        if not serp_key or serp_key == "YOUR_API_KEY":
            names = ", ".join(sorted(_web_real_warned_tasks))
            log(f"[WARNING] SERP_DEV_KEY is not set (or is placeholder "
                f"'YOUR_API_KEY'). {len(_web_real_warned_tasks)} task(s) "
                f"use web_real/web_real_injection service and will get empty search results: "
                f"{names}")
            log("")
        else:
            _web_real_warned_tasks.clear()  # key is set, no warning needed

    # ── Pre-flight: detect tasks whose sandbox_files are missing on disk ───
    # Binary fixtures (videos, etc.) are not shipped in git and must be
    # downloaded separately. Without them inject_files silently injects 0
    # files and the agent fails the task with a confusing 0.2 score. Detect
    # them up front and skip those tasks (counted as errored, excluded from
    # avg_score) instead of burning ~45 min producing meaningless results.
    missing_fixtures = find_missing_fixtures(task_dirs)
    if missing_fixtures:
        log(f"[WARNING] {len(missing_fixtures)} task(s) declare sandbox_files "
            f"that are missing on disk — these tasks will be SKIPPED "
            f"(counted as errored, excluded from avg_score):")
        for tyaml, files in missing_fixtures.items():
            td = task_yaml_map.get(tyaml, os.path.dirname(tyaml))
            tid = os.path.basename(td)
            try:
                tid = load_task_yaml(tyaml).get("task_id", tid) or tid
            except Exception:
                pass
            log(f"  ✗ {tid}: {', '.join(files)}")
        log("  Binary fixtures (videos, etc.) are not shipped in git; download "
            "them per the claw-eval README (Hugging Face: claw-eval/Claw-Eval) "
            "into tasks/<id>/fixtures/, then re-run.")
        log("")

    # ── Chunked execution ────────────────────────────────────────────────
    batch_results = []
    finished_count = 0
    wall_start = time.time()
    results_lock = __import__("threading").Lock()

    # Grade pool is sized independently of exec pool: each grader subprocess
    # hits the judge API (e.g. dashscope chat.completions), and N parallel
    # graders × an internal retry loop will trip rate-limits, causing the
    # graders to silently fall back to 0.0 and produce false-low scores.
    # Default to min(parallel, 2); override with --grade-parallel.
    grade_parallel = getattr(args, "grade_parallel", 0) or min(parallel, 2)
    log(f"Grade pool workers: {grade_parallel} (judge API rate-limit guard)")
    grade_pool = ThreadPoolExecutor(max_workers=grade_parallel)
    grade_futures: dict = {}

    # atexit hook — keeps a reference to the *current* chunk's setup_info
    # so it can clean up if the process dies mid-chunk.
    _current_setup_info = {"ref": None}
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
            kill_mcp_bridges()
            log("[atexit-cleanup] kill_mcp_bridges succeeded")
        except Exception as e:
            log(f"[atexit-cleanup] kill_mcp_bridges failed: {e}")
        si = _current_setup_info["ref"]
        if si:
            try:
                cleanup_parallel_workers(
                    OPENCLAW_CONFIG, si, skip_dirs=False)
                log("[atexit-cleanup] cleanup_parallel_workers succeeded")
            except Exception as e:
                log(f"[atexit-cleanup] cleanup_parallel_workers failed: {e}")
        try:
            restart_gateway(OPENCLAW_CONFIG, gateway_port)
            log("[atexit-cleanup] restart_gateway succeeded")
        except Exception as e:
            log(f"[atexit-cleanup] restart_gateway failed: {e}")
        try:
            reap_orphan_agent_processes()
            log("[atexit-cleanup] reap_orphan_agent_processes succeeded")
        except Exception as e:
            log(f"[atexit-cleanup] reap_orphan_agent_processes failed: {e}")

    atexit.register(_emergency_cleanup)

    def _collect_completed():
        """Check for completed grade futures and log results."""
        nonlocal finished_count
        done_futs = [f for f in grade_futures if f.done()]
        for fut in done_futs:
            task_id, trial, t_submit, sid = grade_futures.pop(fut)
            try:
                result = fut.result()
            except Exception as exc:
                result = {
                    "task_score": 0.0, "passed": False,
                    "completion": 0.0, "robustness": 0.0,
                    "communication": 0.0, "safety": 0.0,
                    "error": str(exc),
                }
            wall_t = time.time() - t_submit
            result["wall_time_s"] = round(wall_t, 2)
            with results_lock:
                finished_count += 1
                fc = finished_count

            entry = {
                "trial": trial,
                "task_score": result["task_score"],
                "passed": result["passed"],
                "completion": result["completion"],
                "robustness": result["robustness"],
                "communication": result["communication"],
                "safety": result["safety"],
                "error": result.get("error"),
                "wall_time_s": result["wall_time_s"],
                "session_id": sid,
                "trace_file": result.get("trace_file"),
                "session_archive_file": result.get("session_archive_file"),
                "session_origin_file": result.get("session_origin_file"),
            }
            with results_lock:
                batch_results.append({"task_id": task_id, "trial": entry})

            status = "PASS" if result["passed"] else "FAIL"
            if result.get("error"):
                status = f"ERROR: {result['error'][:60]}"
            score_str = f"{result['task_score']:.2f}"
            wt = result["wall_time_s"]

            if trials == 1:
                log(f"  [{fc}/{total_runs}] {task_id}: {score_str}  {status} | time=wall {wt:.1f}s")
            else:
                log(f"  [{fc}/{total_runs}] {task_id} trial {trial}: {score_str}  {status} | time=wall {wt:.1f}s")

            elapsed = time.time() - wall_start
            pct = fc / total_runs * 100
            eta = (elapsed / fc) * (total_runs - fc) if fc else 0
            log(f"  [Progress] {fc}/{total_runs} done ({pct:.0f}%) | elapsed {elapsed:.0f}s | ETA ~{int(eta//60)}m{int(eta%60)}s")
            log("")

    def _preserve_session(session_file, task_id, trial):
        """Copy session file to trace_dir so subsequent trial cleanups can't destroy it."""
        stem = f"{task_id}_t{trial}_{os.urandom(4).hex()}"
        safe_path = os.path.join(trace_dir, "sessions", f"{stem}.session.jsonl")
        os.makedirs(os.path.dirname(safe_path), exist_ok=True)
        shutil.copy2(session_file, safe_path)
        return safe_path

    def _execute_one(task_yaml, task_dir, trial, task_slots):
        """Execute a single task run (agent phase). Runs in thread pool."""
        task = load_task_yaml(task_yaml)
        task_id = task["task_id"]

        # Skip tasks whose declared sandbox_files are missing on disk: running
        # them would only burn time and produce a misleading low score.
        if task_yaml in missing_fixtures:
            return {
                "task_id": task_id, "trial": trial,
                "error": "fixture missing: " + ", ".join(missing_fixtures[task_yaml]),
                "wall_time_s": 0.0,
            }

        slot = task_slots[task_yaml]
        agent_id = slot["agent_id"]
        port_offset = slot["port_offset"]

        task_wall_start = time.time()

        try:
            # Unconditional reset — ensures clean state even for trial 1
            reset_services_with_offset(task_yaml, port_offset)

            # Clear stale session files to prevent _find_session_file mtime
            # fallback from returning a previous trial's session
            from ._common import _agent_sessions_dir
            sessions_dir = _agent_sessions_dir(agent_id)
            if os.path.isdir(sessions_dir):
                for f in os.listdir(sessions_dir):
                    if f.endswith(".jsonl"):
                        try:
                            os.remove(os.path.join(sessions_dir, f))
                        except OSError:
                            pass

            session_id = f"claweval-{task_id}-t{trial}-{int(time.time())}-{os.getpid()}"

            ua_enabled = (
                ua_config and ua_config.get("api_key")
                and task.get("user_agent", {}).get("enabled", False)
            )

            # Always-sandbox: per-trial container lifecycle for every task.
            from .sandbox_helpers import start_sandbox_container, stop_sandbox_container

            sandbox_host_port = slot["sandbox_host_port"]
            sandbox_image_slot = slot.get("sandbox_image", "claw-eval-agent:latest")
            run_id = f"{task_id}-t{trial}-{int(time.time())}"
            try:
                handle = start_sandbox_container(
                    image=sandbox_image_slot, run_id=run_id,
                    host_port=sandbox_host_port)
            except Exception as e:
                log(f"[ERROR] {task_id} trial {trial}: sandbox container start failed "
                    f"(image={sandbox_image_slot}, port={sandbox_host_port}): {e}")
                raise
            try:
                # Inject task files into fresh container (no-op when empty).
                runner = slot["sandbox_runner"]
                task_def = slot["task_def"]
                n_injected = runner.inject_files(handle, task_def, task_dir=task_dir)

                # Run agent
                if ua_enabled:
                    session_file = run_agent_with_user_agent(
                        session_id, task_yaml, timeout, ua_config,
                        agent_id=agent_id)
                else:
                    session_file = run_agent(session_id, task_yaml, timeout,
                                            agent_id=agent_id)

                if not session_file:
                    reason = last_agent_error() or "session_file_missing"
                    log(f"[ERROR] {task_id} trial {trial}: agent failed "
                        f"({reason}) (session_id={session_id})")
                    return {
                        "task_id": task_id, "trial": trial,
                        "error": f"agent failed: {reason}",
                        "wall_time_s": round(time.time() - task_wall_start, 2),
                    }

                # Inject grader files AFTER agent loop but BEFORE snapshot
                n_grader = runner.inject_grader_files(handle, task_def,
                                                      task_dir=task_dir)

                # Collect env_snapshot from sandbox container
                task_data = load_task_yaml(task_yaml)
                env_snapshot = collect_env_snapshot(slot["sandbox_url"], task_data)

                snapshot_stem = f"{task_id}_t{trial}_{os.urandom(4).hex()}"
                temp_trace_path = os.path.join(trace_dir, f"{snapshot_stem}.jsonl")
                env_snapshot_path = save_env_snapshot(env_snapshot, temp_trace_path, task_id)

                # Read local grader files
                if hasattr(task_def, "local_grader_files") and task_def.local_grader_files:
                    task_root = Path(task_dir)
                    for rel_path in task_def.local_grader_files:
                        local_path = task_root / rel_path
                        if local_path.exists():
                            content = base64.b64encode(local_path.read_bytes()).decode()
                            env_snapshot[f"local_file:{rel_path}"] = {
                                "encoding": "base64", "content": content,
                            }
                    if env_snapshot_path:
                        with open(env_snapshot_path, "w") as f:
                            json.dump(env_snapshot, f, indent=2, ensure_ascii=False)

                # ── Fetch audit data NOW while mock services still hold
                # this trial's state.  If we defer to the grading phase the
                # next trial's reset will have wiped the data (cross-trial
                # audit contamination — see bug-fix P0).
                audit_data_path = ""
                try:
                    _task_dict = load_task_yaml(task_yaml)
                    _audit = fetch_audit_data(_task_dict, port_offset=port_offset)
                    if _audit:
                        _audit_stem = f"{task_id}_t{trial}_{os.urandom(4).hex()}"
                        _audit_dir = os.path.join(trace_dir, "audit_snapshots")
                        os.makedirs(_audit_dir, exist_ok=True)
                        audit_data_path = os.path.join(_audit_dir, f"{_audit_stem}.json")
                        with open(audit_data_path, "w") as _af:
                            json.dump(_audit, _af, indent=2, ensure_ascii=False)
                        log(f"  [audit] saved {list(_audit.keys())} → {audit_data_path}")
                    del _audit, _task_dict
                except Exception as _audit_exc:
                    log(f"  [audit] WARN: failed to pre-fetch audit data: {_audit_exc}")

                # Release env_snapshot memory — grader reads from file
                del env_snapshot
                import gc
                gc.collect()

                return {
                    "task_id": task_id, "trial": trial,
                    "session_file": _preserve_session(session_file, task_id, trial),
                    "session_id": session_id,
                    "env_snapshot_path": env_snapshot_path,
                    "audit_data_path": audit_data_path,
                    "wall_time_s": round(time.time() - task_wall_start, 2),
                }
            finally:
                stop_sandbox_container(handle)

        except Exception as e:
            log(f"[ERROR] {task_id} trial {trial} execution failed: {e}\n{traceback.format_exc()}")
            return {
                "task_id": task_id, "trial": trial,
                "error": str(e),
                "wall_time_s": round(time.time() - task_wall_start, 2),
            }

    def _execute_task_all_trials(task_yaml, task_dir, n_trials, task_slots):
        """Execute all trials for a single task sequentially."""
        results = []
        for trial in range(1, n_trials + 1):
            result = _execute_one(task_yaml, task_dir, trial, task_slots)
            results.append(result)
        return results

    # ── Chunk loop ────────────────────────────────────────────────────────
    skip_cleanup_dirs = cfg.get("runner", {}).get("skip_cleanup_agent_dirs", False)

    # ── Layer 3: background orphan sweep (best-effort safety net) ─────────
    # Periodically reap claweval- tagged openclaw processes that have been
    # reparented to init (PPID==1). orphans_only avoids killing the in-flight
    # chunk's own processes.
    try:
        sweep_interval = float(os.environ.get("CE_RUNNER_SWEEP_INTERVAL", "60"))
    except ValueError:
        sweep_interval = 60.0
    _sweep_stop = threading.Event()

    def _orphan_sweep_loop():
        while not _sweep_stop.wait(sweep_interval):
            try:
                reap_orphan_agent_processes(orphans_only=True)
            except Exception as e:
                log(f"[orphan-sweep] failed: {e}")

    _sweep_thread = threading.Thread(
        target=_orphan_sweep_loop, name="orphan-sweep", daemon=True)
    _sweep_thread.start()

    for chunk_idx in range(n_chunks):
        chunk_start = chunk_idx * chunk_size
        chunk_end = min(chunk_start + chunk_size, len(task_yamls_unique))
        chunk_tasks = task_yamls_unique[chunk_start:chunk_end]

        log(f"[chunk {chunk_idx + 1}/{n_chunks}] Setting up tasks {chunk_start + 1}-{chunk_end}...")

        # ── Phase 1: Setup this chunk ─────────────────────────────────────
        setup_info = setup_parallel_workers(
            chunk_tasks, parallel, OPENCLAW_CONFIG,
            sandbox_image=sandbox_image,
        )
        task_slots = setup_info["task_slots"]
        _current_setup_info["ref"] = setup_info
        # Reset the cleanup-done flag so atexit can fire for this chunk
        _cleanup_done["done"] = False

        # Start mock services for this chunk's tasks (with port offsets)
        for tyaml, slot in task_slots.items():
            task_dir = task_yaml_map[tyaml]
            log(f"  [setup] Starting mock services for {slot['task_id']} (port_offset={slot['port_offset']})")
            start_mock_services_with_offset(tyaml, task_dir, slot["port_offset"])

        # Restart gateway to pick up this chunk's MCP + agent config
        log(f"[chunk {chunk_idx + 1}/{n_chunks}] Restarting gateway to load chunk config...")
        try:
            gw_ok = restart_gateway(OPENCLAW_CONFIG, gateway_port)
        except RuntimeError as e:
            log(f"[FATAL] {e}")
            sys.exit(1)
        if not gw_ok:
            log("[ERROR] Gateway restart timed out")
            try:
                pgrep = subprocess.run(
                    ["pgrep", "-a", "openclaw"],
                    capture_output=True, timeout=5, text=True)
                log(f"[DIAG] pgrep -a openclaw:\n{pgrep.stdout.strip()}")
            except Exception as _e:
                log(f"[DIAG] pgrep failed: {_e}")
            try:
                ss = subprocess.run(
                    ["ss", "-tlnp", f"sport = :{gateway_port}"],
                    capture_output=True, timeout=5, text=True)
                log(f"[DIAG] ss port {gateway_port}:\n{ss.stdout.strip()}")
            except Exception as _e:
                log(f"[DIAG] ss failed: {_e}")
            sys.exit(1)
        log(f"[chunk {chunk_idx + 1}/{n_chunks}] Gateway ready. Starting execution.\n")

        # ── Phase 2: Parallel execution ───────────────────────────────────
        exec_pool = ThreadPoolExecutor(max_workers=parallel)
        exec_futures = {}

        for tyaml in chunk_tasks:
            task_dir = task_yaml_map[tyaml]
            task = load_task_yaml(tyaml)
            task_id = task["task_id"]
            mode_tag = " [with-files]" if is_sandbox_task(tyaml) else " [empty]"
            log(f"  >> Submitting {task_id}{mode_tag} ({trials} trial{'s' if trials > 1 else ''})")

            fut = exec_pool.submit(_execute_task_all_trials, tyaml, task_dir, trials, task_slots)
            exec_futures[fut] = (tyaml, task_dir)

        # Process execution results as they complete, submit grading
        for fut in as_completed(exec_futures):
            task_yaml, task_dir = exec_futures[fut]
            try:
                trial_results = fut.result()
            except Exception as exc:
                log(f"[ERROR] Task execution future failed: {exc}\n{traceback.format_exc()}")
                trial_results = [{"task_id": "?", "trial": t, "error": str(exc)}
                                for t in range(1, trials + 1)]

            for exec_result in trial_results:
                trial = exec_result.get("trial", 0)
                task_id = exec_result.get("task_id", "?")
                exec_wt = exec_result.get("wall_time_s", 0)
                log(f"  << exec done {task_id} trial {trial} wall={exec_wt:.1f}s")

                if exec_result.get("error"):
                    with results_lock:
                        finished_count += 1
                        fc = finished_count
                    entry = {
                        "trial": trial,
                        "task_score": 0.0, "passed": False,
                        "completion": 0.0, "robustness": 0.0, "communication": 0.0, "safety": 0.0,
                        "error": exec_result["error"],
                        "wall_time_s": exec_result.get("wall_time_s", 0),
                    }
                    with results_lock:
                        batch_results.append({"task_id": task_id, "trial": entry})
                    log(f"  [{fc}/{total_runs}] {task_id}: 0.00  ERROR: {exec_result['error'][:60]}")
                    continue

                # Submit convert+grade
                session_file = exec_result["session_file"]
                sid = exec_result.get("session_id", "")
                t_submit = time.time() - exec_result.get("wall_time_s", 0)

                # Determine port offset for correct audit-data fetching
                _slot = task_slots.get(task_yaml, {})
                _port_offset = _slot.get("port_offset", 0)

                gfut = grade_pool.submit(
                    convert_and_grade_sandbox, task_id, session_file,
                    task_yaml, trace_dir, judge_config,
                    exec_result.get("env_snapshot_path"), sid,
                    mock_port_offset=_port_offset,
                    audit_data_path=exec_result.get("audit_data_path"),
                )
                log(f"  >> grading {task_id} trial {trial}")
                grade_futures[gfut] = (task_id, trial, t_submit, sid)

                _collect_completed()

            # Bridge processes (stdio MCP servers forked by gateway per
            # session) are not auto-disposed — kill them now that this task's
            # trials are done. Grading still needs the mock services HTTP
            # ports for audit-data, but does not need the stdio bridge.
            kill_mcp_bridges(task_yaml)

        exec_pool.shutdown(wait=True)

        # Wait for this chunk's grading to complete before cleanup
        # (grading may need mock services for audit-data fetching)
        while grade_futures:
            time.sleep(0.5)
            _collect_completed()

        # ── Phase 3: Cleanup this chunk ───────────────────────────────────
        log(f"[chunk {chunk_idx + 1}/{n_chunks}] Cleaning up...")
        stop_gateway()
        cleanup_mock_services()
        kill_mcp_bridges()
        cleanup_parallel_workers(
            OPENCLAW_CONFIG, setup_info, skip_dirs=skip_cleanup_dirs)
        reap_orphan_agent_processes()
        _current_setup_info["ref"] = None

        # Release memory between chunks
        import gc
        gc.collect()
        try:
            import ctypes
            libc = ctypes.CDLL("libc.so.6")
            libc.malloc_trim(0)
        except Exception:
            pass

        log(f"[chunk {chunk_idx + 1}/{n_chunks}] Chunk complete.\n")
        time.sleep(3)

    # Mark cleanup done so atexit doesn't repeat
    _cleanup_done["done"] = True

    # Final gateway restart to restore clean state
    try:
        restart_gateway(OPENCLAW_CONFIG, gateway_port)
    except RuntimeError as e:
        log(f"[WARN] Final gateway restart skipped: {e}")
    atexit.unregister(_emergency_cleanup)

    # Stop the background orphan sweep thread
    _sweep_stop.set()
    _sweep_thread.join(timeout=5)

    grade_pool.shutdown(wait=True)

    # ── Aggregate results per task ───────────────────────────────────────
    task_results = {}
    for entry in batch_results:
        tid = entry["task_id"]
        if tid not in task_results:
            task_results[tid] = {"task_id": tid, "trials": []}
        task_results[tid]["trials"].append(entry["trial"])

    total_wall_time = 0.0
    n_pass_at_1 = 0
    n_pass_hat_1 = 0
    n_errored = 0
    score_sum = 0.0
    finished_tasks = 0

    for tid, tr in task_results.items():
        trials_list = tr["trials"]
        n = len(trials_list)
        c = sum(1 for t in trials_list if t["passed"])
        errors = [t for t in trials_list if t.get("error")]

        for t in trials_list:
            total_wall_time += t["wall_time_s"]

        if errors:
            n_errored += 1

        valid = [t for t in trials_list if not t.get("error")]
        avg_score = sum(t["task_score"] for t in valid) / len(valid) if valid else 0.0
        if valid:
            score_sum += avg_score
            finished_tasks += 1

        if c > 0:
            n_pass_at_1 += 1
        if c == n and n > 0:
            n_pass_hat_1 += 1

        tr["avg_score"] = round(avg_score, 4)
        tr["pass_at_1"] = pass_at_k(n, c, 1)
        tr["pass_hat_k"] = (c / n) ** n if n > 0 else 0.0
        tr["avg_passed"] = avg_score >= 0.75
        tr["error"] = errors[0].get("error", "all trials errored") if (errors and not valid) else None
        tr["n"] = n
        tr["c"] = c

    avg_score_final = score_sum / finished_tasks if finished_tasks > 0 else 0.0

    # ── Print summary ────────────────────────────────────────────────────
    log("=" * 60)
    log(f"BATCH COMPLETE -- {len(task_results)} tasks, {parallel} workers, {n_chunks} chunks")
    log("=" * 60)
    log("")
    log(f"  Avg score: {avg_score_final:.3f}")
    log(f"  pass^{trials}: {n_pass_hat_1}/{len(task_results)}")
    log(f"  pass@1: {n_pass_at_1}/{len(task_results)}")
    log(f"  Errored: {n_errored}/{len(task_results)}")
    log(f"  Total time: wall={total_wall_time:.2f}s")
    log("")
    log("\u2500" * 60)

    for tid in sorted(task_results.keys()):
        tr = task_results[tid]
        avg = tr["avg_score"]
        status = "PASS" if avg >= 0.75 else "FAIL"
        has_error = any(t.get("error") for t in tr["trials"])
        if has_error and not any(not t.get("error") for t in tr["trials"]):
            status = "ERROR"

        last = tr["trials"][-1]
        c_s = last["completion"]
        r_s = last["robustness"]
        m_s = last["communication"]
        s_s = last["safety"]
        wt = sum(t["wall_time_s"] for t in tr["trials"])

        label = f"  {tid:<40s}  {avg:.2f}  {status}"
        if tr["n"] > 1:
            trial_scores = "/".join(f"{t['task_score']:.2f}" for t in tr["trials"])
            label += f"  trials=[{trial_scores}]"
            label += f"  pass^{tr['n']}={'Y' if tr['c'] == tr['n'] else 'N'}"
            label += f"  pass@{tr['n']}={'Y' if tr['c'] > 0 else 'N'}"
        else:
            label += f"  C={c_s:.2f} R={r_s:.2f} M={m_s:.2f} S={s_s:.2f}"
        label += f"  TIME=wall {wt:.1f}s"
        log(label)

    log("\u2500" * 60)
    log(f"Trace dir: {trace_dir}")

    # ── Write batch_results.json ─────────────────────────────────────────
    results_file = os.path.join(trace_dir, "batch_results.json")
    with open(results_file, "w") as f:
        json.dump([
            {
                "task_id": tr["task_id"],
                "task_name": task_meta.get(tr["task_id"], {}).get("task_name", ""),
                "difficulty": task_meta.get(tr["task_id"], {}).get("difficulty", ""),
                "trials": tr["trials"],
                "error": tr["error"],
                "avg_score": tr["avg_score"],
                "pass_at_1": tr["pass_at_1"],
                "pass_hat_k": tr["pass_hat_k"],
                "avg_passed": tr["avg_passed"],
                "task_note": (
                    ["SERP_DEV_KEY not set — web_search returns empty results"]
                    if tr["task_id"] in _web_real_warned_tasks else []
                ),
            }
            for tr in task_results.values()
        ], f, indent=2, ensure_ascii=False)

    # ── Write batch_summary.json ─────────────────────────────────────────
    summary_file = os.path.join(trace_dir, "batch_summary.json")
    with open(summary_file, "w") as f:
        json.dump({
            "tasks": len(task_results),
            "trials_per_task": trials,
            "chunk_size": chunk_size,
            "n_chunks": n_chunks,
            f"pass_hat_{trials}": n_pass_hat_1,
            f"pass_at_{trials}": n_pass_at_1,
            "errored": n_errored,
            "avg_score": round(avg_score_final, 4),
            "total_wall_time_s": round(total_wall_time, 2),
        }, f, indent=2)

    log(f"Results: {results_file}")

    # ── Write session_map.json (trace ↔ openclaw session mapping) ───────────
    session_map_entries = []
    for entry in batch_results:
        tid = entry["task_id"]
        t = entry["trial"]
        trace_path = t.get("trace_file")
        archive_path = t.get("session_archive_file")
        if not trace_path and not archive_path:
            continue
        session_map_entries.append({
            "task_id": tid,
            "trial": t.get("trial"),
            "session_id": t.get("session_id", ""),
            "trace_file": (os.path.relpath(trace_path, trace_dir)
                           if trace_path else ""),
            "session_file": (os.path.relpath(archive_path, trace_dir)
                             if archive_path else ""),
            "original_session_path": t.get("session_origin_file") or "",
        })
    session_map_file = os.path.join(trace_dir, "session_map.json")
    with open(session_map_file, "w") as f:
        json.dump({
            "trace_dir": trace_dir,
            "sessions_dir": "sessions",
            "entries": session_map_entries,
        }, f, indent=2, ensure_ascii=False)
    log(f"Session map: {session_map_file}")

    detach_log_file()
