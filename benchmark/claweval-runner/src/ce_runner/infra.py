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

"""Infrastructure helpers: gateway, mock services, agent config, cleanup."""

from __future__ import annotations

import json
import os
import shutil
import signal
import subprocess
import time
from pathlib import Path
from typing import Any

import httpx

from ._common import (OPENCLAW_CONFIG, RESET_PATHS, _PYTHON,
                       atomic_write_config, load_task_yaml, log)
from .parallel import (reset_services_with_offset,
                       start_mock_services_with_offset)
from .skill_generator import cleanup_task_skill
from .tool_injector import ToolInjector


# ── Gateway ────────────────────────────────────────────────────────────────────


def ensure_user_session_persistent() -> None:
    """Enable loginctl linger so /run/user/{uid} survives detached sessions."""
    try:
        user = (os.environ.get("USER")
                or subprocess.check_output(["id", "-un"], text=True).strip())
    except Exception:
        return

    try:
        result = subprocess.run(
            ["loginctl", "show-user", user, "--property=Linger"],
            capture_output=True, text=True, timeout=5)
        if result.returncode == 0 and "Linger=yes" in result.stdout:
            return
    except Exception as e:
        log(f"[WARNING] Could not query linger status: {e}. "
            "Run 'loginctl enable-linger' manually to avoid "
            "session cleanup during long batch runs.")
        return

    try:
        subprocess.run(
            ["loginctl", "enable-linger", user],
            check=True, capture_output=True, timeout=10)
        log(f"[setup] Enabled loginctl linger for user '{user}'")
    except Exception as e:
        log(f"[WARNING] Could not enable linger: {e}. "
            "Run 'loginctl enable-linger' manually to avoid "
            "session cleanup during long batch runs.")


def _gateway_env() -> dict[str, str]:
    """Build env dict ensuring user-bus access for openclaw gateway commands."""
    env = os.environ.copy()
    uid = os.getuid()
    runtime_dir = f"/run/user/{uid}"
    env.setdefault("XDG_RUNTIME_DIR", runtime_dir)
    env.setdefault("DBUS_SESSION_BUS_ADDRESS", f"unix:path={runtime_dir}/bus")
    return env


def _check_user_bus(env: dict) -> None:
    """Pre-flight check: verify user-bus socket is reachable.

    Raises RuntimeError with actionable diagnostics if not.
    """
    runtime_dir = env.get("XDG_RUNTIME_DIR", "")
    bus_path = os.path.join(runtime_dir, "bus")
    issues: list[str] = []
    if not os.path.isdir(runtime_dir):
        issues.append(f"XDG_RUNTIME_DIR={runtime_dir} does not exist")
    elif not os.path.exists(bus_path):
        issues.append(f"D-Bus socket not found: {bus_path}")
    if issues:
        msg = (
            "User-bus pre-check failed — openclaw gateway commands will not work.\n"
            f"  {'; '.join(issues)}\n"
            f"  Env: XDG_RUNTIME_DIR={runtime_dir}, "
            f"DBUS_SESSION_BUS_ADDRESS={env.get('DBUS_SESSION_BUS_ADDRESS', '<unset>')}\n"
            "  Fix: ensure 'systemctl --user' works in this shell, "
            "or start batch from a login session (ssh / machinectl shell)."
        )
        raise RuntimeError(msg)


def check_gateway(config_path: str) -> int:
    with open(config_path) as f:
        config = json.load(f)
    port = config["gateway"]["port"]
    try:
        r = httpx.get(f"http://127.0.0.1:{port}/health", timeout=3)
        if r.status_code == 200:
            return port
    except Exception as e:
        log(f"[DEBUG] gateway health probe failed: {e}")
    return 0


def stop_gateway():
    """Stop gateway before cleanup to eliminate file-watcher races."""
    env = _gateway_env()
    result = subprocess.run(
        ["openclaw", "gateway", "stop"],
        capture_output=True, timeout=30, env=env)
    if result.returncode != 0:
        stderr = result.stderr.decode(errors="replace").strip()
        log(f"[WARN] gateway stop exited with code {result.returncode}: {stderr}")
    time.sleep(2)


def restart_gateway(config_path: str, gateway_port: int):
    """Restart openclaw gateway to pick up new config."""
    env = _gateway_env()
    _check_user_bus(env)

    with open(config_path) as f:
        config = json.load(f)
    if "gateway" not in config or "mode" not in config.get("gateway", {}):
        config.setdefault("gateway", {})["mode"] = "local"
        with open(config_path, "w") as f:
            json.dump(config, f, indent=2)

    # Use 'restart' instead of 'stop' + 'start' — stop removes agent config
    result = subprocess.run(["openclaw", "gateway", "restart"], capture_output=True, timeout=30, env=env)
    if result.returncode != 0:
        stderr = result.stderr.decode(errors="replace").strip()
        stdout = result.stdout.decode(errors="replace").strip()
        log(f"[WARN] gateway restart exited with code {result.returncode}: {stderr}")
        if stdout:
            log(f"[WARN] gateway restart stdout: {stdout}")

    time.sleep(2)

    health_url = f"http://127.0.0.1:{gateway_port}/health"
    last_err = None
    for _ in range(45):
        try:
            r = httpx.get(health_url, timeout=3)
            if r.status_code == 200:
                time.sleep(8)
                return True
            last_err = f"HTTP {r.status_code}"
        except Exception as e:
            last_err = str(e)
        time.sleep(1)

    log(f"[ERROR] gateway health check failed after 45s (last error: {last_err})")
    return False


# ── Tool / agent configuration ─────────────────────────────────────────────────

def configure_tools(
    task_yaml: str,
    port_offset: int = 0,
    sandbox_url: str = None,
) -> tuple[str, ToolInjector, Any]:
    """Register per-task MCP server and configure agent.

    When sandbox_url is provided, sandbox tools are also registered.

    Returns (agent_id, injector, context) so the caller can clean up later.
    """
    injector = ToolInjector(OPENCLAW_CONFIG)
    ctx = injector.configure(task_yaml, port_offset=port_offset, sandbox_url=sandbox_url)
    return ctx.agent_id, injector, ctx


# ── Mock service lifecycle ─────────────────────────────────────────────────────

def cleanup_mock_services():
    """Kill all mock service processes."""
    result = subprocess.run(["ps", "auxww"], capture_output=True, text=True)
    my_pid = os.getpid()
    for line in result.stdout.splitlines():
        if "mock_services" in line and "grep" not in line:
            parts = line.split()
            if len(parts) > 1:
                pid = int(parts[1])
                if pid != my_pid:
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except Exception:
                        pass


def reap_orphan_agent_processes(markers: list[str] | None = None,
                                orphans_only: bool = False):
    """Reap stray openclaw agent processes tagged with a claw-eval marker.

    Scans ``ps -eo pid,ppid,args`` and terminates any process whose argv
    contains both ``"openclaw"`` and one of *markers* (default
    ``["claweval-"]``).  Also covers openclaw MCP stdio bridge processes
    (``python -m ce_runner.mcp_mock_services`` /
    ``ce_runner.mcp_sandbox_tools``) which have no ``openclaw``/``claweval-``
    tokens in their argv.  Termination is by process group (SIGTERM) so that
    detached grandchildren are swept too, falling back to ``os.kill`` when the
    pgid cannot be resolved.  Best-effort: per-process failures are swallowed
    so one stuck pid never blocks the rest.

    When *orphans_only* is True only processes that have been reparented to
    init (PPID == 1) are reaped — used by the conservative background sweep to
    avoid killing processes belonging to the in-flight chunk.
    """
    if markers is None:
        markers = ["claweval-"]

    try:
        result = subprocess.run(
            ["ps", "-ww", "-eo", "pid,ppid,args"],
            capture_output=True, text=True, timeout=10)
    except Exception as e:
        log(f"[reap-orphan] ps failed: {e}")
        return

    my_pid = os.getpid()
    reaped: list[int] = []
    for line in result.stdout.splitlines()[1:]:  # skip header
        parts = line.split(maxsplit=2)
        if len(parts) < 3:
            continue
        try:
            pid = int(parts[0])
            ppid = int(parts[1])
        except ValueError:
            continue
        args = parts[2]

        if pid == my_pid:
            continue
        is_openclaw_match = ("openclaw" in args
                             and any(m in args for m in markers))
        is_bridge_match = ("ce_runner.mcp_mock_services" in args
                           or "ce_runner.mcp_sandbox_tools" in args)
        if not (is_openclaw_match or is_bridge_match):
            continue
        if orphans_only and ppid != 1:
            continue

        try:
            try:
                os.killpg(os.getpgid(pid), signal.SIGTERM)
            except (ProcessLookupError, PermissionError):
                continue
            except OSError:
                os.kill(pid, signal.SIGTERM)
            reaped.append(pid)
        except (ProcessLookupError, PermissionError):
            pass

    if reaped:
        log(f"[reap-orphan] reaped pids: {reaped} "
            f"(orphans_only={orphans_only})")


def kill_mcp_bridges(task_yaml: str | None = None) -> int:
    """Reap leaked openclaw MCP stdio bridge processes.

    The gateway forks ``python -m ce_runner.mcp_mock_services`` and
    ``python -m ce_runner.mcp_sandbox_tools`` per agent session but never
    disposes them after a run (the ``openclaw agent`` CLI does not forward
    ``cleanupBundleMcpOnRunEnd`` to the gateway).  These bridges accumulate
    across a chunk and consume a lot of memory.

    When *task_yaml* is provided, only bridges whose argv references that
    exact absolute path are killed — safe to call from a per-task completion
    callback while other tasks are still in flight.  When None, all matching
    bridges are killed (used at chunk boundary / emergency cleanup).

    Best-effort SIGTERM to the process group; mirrors
    :func:`reap_orphan_agent_processes` style.  Returns the number of pids
    terminated.
    """
    target_path = os.path.abspath(task_yaml) if task_yaml else None
    try:
        result = subprocess.run(
            ["ps", "-ww", "-eo", "pid,ppid,pgid,args"],
            capture_output=True, text=True, timeout=10)
    except Exception as e:
        log(f"[mcp-bridge-cleanup] ps failed: {e}")
        return 0

    my_pid = os.getpid()
    reaped: list[int] = []
    for line in result.stdout.splitlines()[1:]:  # skip header
        parts = line.split(maxsplit=3)
        if len(parts) < 4:
            continue
        try:
            pid = int(parts[0])
            pgid = int(parts[2])
        except ValueError:
            continue
        args = parts[3]

        if pid == my_pid or pid == 1:
            continue
        if ("ce_runner.mcp_mock_services" not in args
                and "ce_runner.mcp_sandbox_tools" not in args):
            continue
        if target_path is not None and target_path not in args:
            continue

        try:
            try:
                os.killpg(pgid, signal.SIGTERM)
            except (ProcessLookupError, PermissionError):
                continue
            except OSError:
                os.kill(pid, signal.SIGTERM)
            reaped.append(pid)
        except (ProcessLookupError, PermissionError):
            pass

    if reaped:
        log(f"[mcp-bridge-cleanup] reaped {len(reaped)} pids "
            f"(task_yaml={task_yaml})")
    return len(reaped)


def reset_services(task_yaml: str = None):
    """Reset remaining services via reset endpoints.

    If *task_yaml* is given, reset endpoints are read from the task YAML
    (preferred).  Otherwise falls back to the hardcoded ``RESET_PATHS``.
    """
    if task_yaml:
        reset_services_with_offset(task_yaml, port_offset=0)
        return
    for port, path in RESET_PATHS.items():
        try:
            httpx.post(f"http://localhost:{port}{path}", json={}, timeout=3)
        except Exception:
            pass


def start_mock_services(task_yaml: str, task_dir: str):
    """Start mock services required by the task (port_offset=0)."""
    start_mock_services_with_offset(task_yaml, task_dir, port_offset=0)


# ── Cleanup ────────────────────────────────────────────────────────────────────

def cleanup_session(session_file: str, session_id: str = ""):
    """Remove openclaw session file and associated artifacts.

    Cleans up:
    1. The session JSONL file in SESSIONS_DIR (UUID-named)
    2. Associated checkpoint/reset/deleted files in the same directory
    3. Temporary /tmp/openclaw_agent_* result JSON and error log files

    Called after trace conversion succeeds, since the session data has been
    converted to a claw-eval trace and is no longer needed.
    """
    # 1. Remove session JSONL and associated files (checkpoint, deleted, reset)
    if session_file and os.path.exists(session_file):
        session_path = Path(session_file)
        session_dir = session_path.parent
        stem = session_path.stem  # UUID, e.g. "04df1b8b-..."
        try:
            session_path.unlink(missing_ok=True)
            for f in session_dir.glob(f"{stem}.*"):
                f.unlink(missing_ok=True)
        except Exception:
            pass

    # 2. Remove /tmp files keyed by session_id
    if session_id:
        for pattern in [
            f"openclaw_agent_result_{session_id}*.json",
            f"openclaw_agent_{session_id}*.err",
        ]:
            try:
                for f in Path("/tmp").glob(pattern):
                    f.unlink(missing_ok=True)
            except Exception:
                pass


def cleanup_config(context=None, config_path: str = None, skip_dirs: bool = False):
    """Remove task agents and clean up generated files.

    If *context* is provided (ToolInjectionContext from configure_tools),
    uses it for targeted cleanup. Otherwise falls back to cleaning all
    claweval- agents (backward compat for batch mode).

    Config mutations are done via direct JSON manipulation + atomic write,
    bypassing openclaw's ``writeConfigFile`` which has a size-drop gate that
    rejects writes shrinking the file by >50% — a common scenario when
    removing agents with large tool allow/deny lists.

    When *skip_dirs* is False (the default), also removes leftover agent
    directories from disk (*~/.openclaw/agents/claweval-*/).
    Set *skip_dirs* to True via config option ``runner.skip_cleanup_agent_dirs``
    to keep session artefacts for debugging.
    """
    if context is not None:
        # Targeted cleanup for a single task
        injector = ToolInjector(OPENCLAW_CONFIG)
        injector.cleanup(context, skip_dirs=skip_dirs)
        return

    # Fallback: clean all claweval- agents (batch mode)
    cleanup_path = config_path or OPENCLAW_CONFIG
    try:
        with open(cleanup_path) as f:
            config = json.load(f)

        agents_list = config.get("agents", {}).get("list", [])
        removed_ids = [a["id"] for a in agents_list if a.get("id", "").lower().startswith("claweval-")]

        # Batch-remove agents from config
        config.setdefault("agents", {})["list"] = [
            a for a in agents_list if a.get("id", "").lower() not in
            {aid.lower() for aid in removed_ids}
        ]

        # Remove all ce-runner MCP server entries (ce-mock-*, ce-sb-*, claw-eval-*)
        servers = config.get("mcp", {}).get("servers", {})
        stale_keys = [k for k in servers
                      if k.startswith(("claw-eval-", "ce-mock-", "ce-sb-"))]
        for k in stale_keys:
            del servers[k]

        config["tools"] = {"profile": "coding"}
        atomic_write_config(cleanup_path, config)

        for aid in removed_ids:
            task_id = aid[len("claweval-"):]
            cleanup_task_skill(task_id)

        if not skip_dirs:
            agents_root = Path(cleanup_path).parent / "agents"
            workspace_root = Path(cleanup_path).parent
            for aid in removed_ids:
                agent_dir = agents_root / aid.lower()
                workspace_dir = workspace_root / f"workspace-{aid.lower()}"
                for d in (agent_dir, workspace_dir):
                    if d.is_dir():
                        shutil.rmtree(d, ignore_errors=True)
    except Exception:
        pass
