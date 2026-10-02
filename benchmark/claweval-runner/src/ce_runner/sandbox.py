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

"""Sandbox (Docker-based) task execution."""

from __future__ import annotations

import base64
import json
import os
import time
from pathlib import Path

import httpx

from .agent import run_agent, run_agent_with_user_agent
from ._common import (DEFAULT_AGENT_TIMEOUT_S, OPENCLAW_CONFIG, load_task_yaml,
                       log)


def _normalize_read_response(data: dict) -> dict:
    """Convert media-format /read responses to the standard encoding/content format.

    The /read endpoint returns a media-format response for image files
    (with "frames" containing image_b64) instead of the simple
    {"encoding": "base64", "content": ...} that save_env_snapshot and graders
    (VisualGraderMixin.collect_screenshots_from_snapshot) expect. Extract the
    raw image data from the first frame so downstream consumers see a uniform
    schema. Non-media responses are returned unchanged.
    """
    if isinstance(data, dict) and data.get("frames") and not data.get("encoding"):
        frames = data["frames"]
        if frames and frames[0].get("image_b64"):
            return {
                "content": frames[0]["image_b64"],
                "encoding": "base64",
                "mime_type": frames[0].get("mime_type", "image/png"),
            }
    return data


def collect_env_snapshot(sandbox_url: str, task_data: dict) -> dict:
    """Collect environment data from sandbox container after agent loop.

    Runs env_snapshot_commands and reads env_snapshot_files declared in task.yaml.
    """
    timeout = task_data.get("environment", {}).get("env_snapshot_timeout", 10)
    client = httpx.Client(timeout=max(timeout + 5, 15.0))
    snapshot: dict = {}

    try:
        # Run commands first (they may generate files we need to collect)
        for cmd in task_data.get("env_snapshot_commands", []):
            try:
                resp = client.post(
                    f"{sandbox_url}/exec",
                    json={"command": cmd, "timeout_seconds": timeout},
                )
                cmd_result = resp.json()
                snapshot[f"cmd:{cmd}"] = cmd_result
                exit_code = cmd_result.get("exit_code", "?")
                stderr = (cmd_result.get("stderr") or "")[:200]
                log(f"  [env_snapshot] cmd exit={exit_code}: {cmd[:80]}")
                if stderr:
                    log(f"  [env_snapshot]   stderr: {stderr}")
            except Exception as exc:
                snapshot[f"cmd:{cmd}"] = {"error": str(exc)}
                log(f"  [WARNING] env_snapshot command failed: {cmd}: {exc}")

        # Collect files
        for pattern in task_data.get("env_snapshot_files", []):
            try:
                if "*" in pattern or "?" in pattern:
                    resp = client.post(
                        f"{sandbox_url}/glob",
                        json={"pattern": pattern, "max_files": 50},
                    )
                    file_list = resp.json().get("files", [])
                    log(f"  [env_snapshot] glob '{pattern}' -> {len(file_list)} file(s)")
                    for f_entry in file_list:
                        try:
                            resp2 = client.post(
                                f"{sandbox_url}/read",
                                json={"path": f_entry["path"]},
                            )
                            snapshot[f"file:{f_entry['path']}"] = _normalize_read_response(resp2.json())
                        except Exception as exc:
                            snapshot[f"file:{f_entry['path']}"] = {"error": str(exc)}
                else:
                    resp = client.post(
                        f"{sandbox_url}/read",
                        json={"path": pattern},
                    )
                    snapshot[f"file:{pattern}"] = _normalize_read_response(resp.json())
            except Exception as exc:
                snapshot[f"file:{pattern}"] = {"error": str(exc)}
                log(f"  [WARNING] env_snapshot file failed: {pattern}: {exc}")
    finally:
        client.close()

    return snapshot


def save_env_snapshot(snapshot: dict, trace_path: str, task_id: str) -> str:
    """Save env_snapshot to JSON file alongside the trace. Returns the JSON path."""
    if not snapshot:
        return ""

    snapshot_dir = Path(trace_path).parent / f"{Path(trace_path).stem}_snapshot"
    snapshot_dir.mkdir(parents=True, exist_ok=True)

    # Save the full snapshot as JSON for grader
    snapshot_json = str(snapshot_dir / "env_snapshot.json")
    with open(snapshot_json, "w") as f:
        json.dump(snapshot, f, indent=2, ensure_ascii=False)

    # Also save binary files (screenshots, etc.)
    for key, entry in sorted(snapshot.items()):
        if key.startswith("file:"):
            file_path = key[len("file:"):]
            if isinstance(entry, dict) and entry.get("encoding") == "base64" and entry.get("content"):
                filename = Path(file_path).name
                out_path = snapshot_dir / filename
                try:
                    out_path.write_bytes(base64.b64decode(entry["content"]))
                except Exception:
                    pass

    return snapshot_json


def execute_task_sandbox(task_yaml: str, task_dir: str, trace_dir: str,
                         model_config: dict, sandbox_image: str = None,
                         timeout: int = DEFAULT_AGENT_TIMEOUT_S,
                         gateway_port: int = 0,
                         ua_config: dict = None) -> dict:
    """Execute a sandbox task using openclaw agent with Docker container."""
    from .infra import (cleanup_mock_services, configure_tools,
                        reset_services, restart_gateway,
                        start_mock_services)
    from claw_eval.config import SandboxConfig
    from claw_eval.models.task import TaskDefinition
    from claw_eval.runner.sandbox_runner import SandboxRunner

    task_def = TaskDefinition.from_yaml(task_yaml)
    task_id = task_def.task_id
    task_data = load_task_yaml(task_yaml)

    try:
        # --- Start Docker container ---
        sandbox_cfg = SandboxConfig(
            image=sandbox_image or "claw-eval-agent:latest",
        )
        runner = SandboxRunner(sandbox_cfg)

        run_id = f"{task_id}-{int(time.time())}-{os.getpid()}"
        log(f"  [sandbox] Starting container claw-agent-{run_id}...")
        handle = runner.start_container(run_id=run_id)
        sandbox_url = handle.sandbox_url

        try:
            # --- Inject task files ---
            n_injected = runner.inject_files(handle, task_def, task_dir=task_dir)
            expected = len(task_def.sandbox_files) if task_def.sandbox_files else 0
            log(f"  [sandbox] Injected {n_injected}/{expected} files into container")
            if expected and n_injected < expected:
                log(f"  [WARNING] inject_files: only {n_injected}/{expected} files injected")

            # --- Configure MCP with sandbox bridge ---
            agent_id, _injector, _ctx = configure_tools(task_yaml, sandbox_url=sandbox_url)
            cleanup_mock_services()
            reset_services(task_yaml)
            start_mock_services(task_yaml, task_dir)

            try:
                if not restart_gateway(OPENCLAW_CONFIG, gateway_port):
                    return {"task_id": task_id, "error": "Gateway restart timed out"}
            except RuntimeError as e:
                return {"task_id": task_id, "error": f"Gateway user-bus check failed: {e}"}

            # --- Run openclaw agent ---
            session_id = f"claweval-{task_id}-{int(time.time())}-{os.getpid()}"

            # Use multi-round dialogue for user_agent tasks
            ua_enabled = (
                ua_config
                and ua_config.get("api_key")
                and task_data.get("user_agent", {}).get("enabled", False)
            )
            if ua_enabled:
                session_file = run_agent_with_user_agent(
                    session_id, task_yaml, timeout, ua_config, agent_id=agent_id)
            else:
                session_file = run_agent(session_id, task_yaml, timeout,
                                         agent_id=agent_id)

            if not session_file:
                return {"task_id": task_id, "error": "No session file found"}

            # --- Inject grader-only files AFTER agent loop ---
            n_grader = runner.inject_grader_files(handle, task_def, task_dir=task_dir)
            if task_def.sandbox_grader_files and n_grader < len(task_def.sandbox_grader_files):
                log(f"  [WARNING] inject_grader_files: only {n_grader}/{len(task_def.sandbox_grader_files)} files injected")

            # --- Collect env snapshot before destroying container ---
            env_snapshot = collect_env_snapshot(sandbox_url, task_data)
            # Use a temp path for snapshot (will be updated with actual trace path later)
            snapshot_stem = f"{task_id}_{os.urandom(4).hex()}"
            temp_trace_path = os.path.join(trace_dir, f"{snapshot_stem}.jsonl")
            env_snapshot_path = save_env_snapshot(env_snapshot, temp_trace_path, task_id)

            # Read local grader files from host
            if hasattr(task_def, "local_grader_files") and task_def.local_grader_files:
                task_root = Path(task_dir)
                for rel_path in task_def.local_grader_files:
                    local_path = task_root / rel_path
                    if local_path.exists():
                        content = base64.b64encode(local_path.read_bytes()).decode()
                        env_snapshot[f"local_file:{rel_path}"] = {
                            "encoding": "base64",
                            "content": content,
                        }
                # Re-save with local files
                if env_snapshot_path:
                    with open(env_snapshot_path, "w") as f:
                        json.dump(env_snapshot, f, indent=2, ensure_ascii=False)

        finally:
            cleanup_mock_services()
            runner.stop_container(handle)

        return {
            "task_id": task_id,
            "session_file": session_file,
            "session_id": session_id,
            "env_snapshot_path": env_snapshot_path,
        }

    except Exception as e:
        return {"task_id": task_id, "error": str(e)}


def convert_and_grade_sandbox(task_id: str, session_file: str, task_yaml: str,
                               trace_dir: str, judge_config: dict,
                               env_snapshot_path: str = None,
                               session_id: str = "",
                               mock_port_offset: int = 0,
                               audit_data_path: str = None) -> dict:
    """Convert openclaw session trace and grade for a sandbox task.

    *audit_data_path* (optional) points to a pre-saved audit JSON file.
    When provided, the converter reads this file instead of fetching live
    mock services, preventing cross-trial audit contamination.
    """
    from .infra import cleanup_session
    from .pipeline import phase_convert, phase_grade
    result = {
        "task_id": task_id, "task_score": 0.0, "passed": False,
        "completion": 0.0, "robustness": 0.0, "communication": 0.0, "safety": 0.0,
        "error": None, "trace_file": None,
    }
    try:
        trace_file = os.path.join(trace_dir, f"{task_id}_{os.urandom(4).hex()}.jsonl")
        if not phase_convert(session_file, task_yaml, trace_file,
                             mock_port_offset=mock_port_offset,
                             audit_data_path=audit_data_path):
            result["error"] = "Trace conversion failed"
            return result

        # Archive openclaw session alongside trace BEFORE cleanup deletes it.
        from .pipeline import archive_session_alongside_trace
        result["session_archive_file"] = archive_session_alongside_trace(
            session_file, trace_file)
        result["session_origin_file"] = session_file

        # Session converted to trace — clean up session files
        cleanup_session(session_file, session_id)

        scores = phase_grade(trace_file, task_yaml, judge_config,
                             env_snapshot_path=env_snapshot_path)
        result.update(scores)
        result["trace_file"] = trace_file
    except Exception as e:
        result["error"] = str(e)
    return result
