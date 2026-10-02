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

"""Unified tool injection layer for claw-eval tasks.

Registers per-task MCP servers and agent configs by writing directly to
``openclaw.json`` in a single atomic operation, then the caller restarts
the gateway once to pick up all changes.  This avoids the race condition
where ``openclaw mcp set`` CLI triggers an intermediate gateway restart
before agent configs are written.

Key conventions (from openclaw native MCP runtime):
  - MCP servers registered in config persist across gateway restarts
  - Tool names follow ``<serverKey>__<toolName>`` format
  - Agent uses ``tools.alsoAllow`` to make MCP tools visible (``tools.allow``
    cannot expose MCP tools — gateway limitation)
  - Agent uses ``tools.deny`` to block all built-in tools (exec, read, write,
    etc.) so agent CANNOT access host filesystem (anti-cheat isolation)
  - ``tools.exec`` policy enables MCP tool execution through the bridge
  - No ``openclaw-mcp-adapter`` plugin needed

Usage:
    # Single task
    injector = ToolInjector(OPENCLAW_CONFIG)
    ctx = injector.configure(task_yaml, port_offset=0)
    # ... run agent ...
    injector.cleanup(ctx)

    # Batch tasks
    injector = ToolInjector(OPENCLAW_CONFIG)
    setup_info = injector.setup_parallel_workers(task_yamls, parallel)
    # ... run agents in parallel ...
    injector.cleanup_parallel(setup_info)
"""

from __future__ import annotations

import json
import os
import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from ._common import (OPENCLAW_CONFIG, _PYTHON, _agent_sessions_dir,
                       atomic_write_config, init_config_defaults,
                       load_task_yaml, log,
                       mock_mcp_name, sandbox_mcp_name, task_agent_id)
from .skill_generator import cleanup_task_skill

# Fixed sandbox tool names (from mcp_sandbox_tools.py)
_SANDBOX_TOOL_NAMES = ["Bash", "Read", "Write", "Edit", "Glob", "Grep",
                        "BrowserScreenshot", "ReadMedia", "Download"]

# Built-in gateway tools to deny — prevents agent from accessing host filesystem.
# These are the default tools openclaw exposes; blocking them forces the agent
# to use only the MCP-bridged sandbox tools (which route to the Docker container).
_DENY_BUILTIN_TOOLS = [
    "exec", "read", "write", "edit", "process", "browser", "canvas",
    "nodes", "cron", "sessions_list", "sessions_history", "sessions_send",
    "sessions_spawn", "sessions_yield", "subagents", "web_fetch",
    "session_status", "memory_get", "memory_search",
]


def build_agent_tools(
    mock_mcp_name: str | None = None,
    mock_tool_names: list[str] | None = None,
    sandbox_mcp_name: str | None = None,
    extra_deny: list[str] | None = None,
    allowed: list[str] | None = None,
) -> dict:
    """Build the complete agent ``tools`` dict (allow + deny + exec policy).

    Stateless, shared helper used by both production injection code and tests
    so the agent configuration cannot drift between them.

    Args:
        mock_mcp_name: Mock MCP server key (for building the allow list).
        mock_tool_names: Mock tool names exposed by the task.
        sandbox_mcp_name: Sandbox MCP server key (for building the allow list).
        extra_deny: Additional deny entries (e.g. cross-task ``<serverKey>__*``
            wildcards). Appended after the built-in deny list.
        allowed: Pre-built ``serverKey__toolName`` allow entries. If provided,
            it is used directly instead of calling ``_build_allowlist`` with the
            ``mock_*`` / ``sandbox_*`` arguments.

    Returns:
        ``{"allow": [...], "deny": [...], "exec": {...}}`` ready to drop into an
        agent's ``tools`` config.
    """
    if allowed is None:
        allowed = ToolInjector._build_allowlist(
            mock_mcp_name, mock_tool_names, sandbox_mcp_name,
        )
    deny = list(_DENY_BUILTIN_TOOLS)
    if extra_deny:
        deny.extend(extra_deny)
    return {
        "allow": ["exec", *allowed],
        "deny": deny,
        "exec": {
            "security": "full",
            "ask": "off",
        },
    }


@dataclass
class ToolInjectionContext:
    """Tracks resources created during tool injection for cleanup."""
    task_id: str
    agent_id: str
    port_offset: int = 0
    mcp_name: str | None = None
    sandbox_url: str | None = None
    sandbox_mcp_name: str | None = None


class ToolInjector:
    """Tool injector that registers per-task MCP servers in openclaw config.

    Args:
        config_path: Path to openclaw.json config file.
    """

    def __init__(self, config_path: str, **kwargs):
        self.config_path = config_path

    def configure(
        self,
        task_yaml: str,
        port_offset: int = 0,
        sandbox_url: str | None = None,
    ) -> ToolInjectionContext:
        """Register tools into openclaw config.

        Writes MCP servers and agent config directly to openclaw.json in a
        single atomic write, avoiding CLI-triggered gateway restarts.

        Args:
            task_yaml: Path to the task's task.yaml file.
            port_offset: Port offset for MCP server (used in batch mode).
            sandbox_url: If provided, also registers sandbox MCP server.

        Returns:
            ToolInjectionContext describing the injected resources.
        """
        task = load_task_yaml(task_yaml)
        task_id = task["task_id"]
        agent_id = task_agent_id(task_id)
        mcp_name = mock_mcp_name(task_id)
        sandbox_mcp = None

        # Step 1: Create agent via CLI
        workspace = Path.home() / ".openclaw" / f"workspace-{agent_id.lower()}"
        subprocess.run([
            "openclaw", "agents", "add", agent_id,
            "--workspace", str(workspace),
            "--non-interactive",
        ], capture_output=True, timeout=15)

        # Step 2: Collect MCP server configs
        init_config_defaults()
        mcp_servers: dict[str, dict] = {}
        mcp_servers[mcp_name] = {
            "command": _PYTHON,
            "args": [
                "-m", "ce_runner.mcp_mock_services",
                "--task-yaml", os.path.abspath(task_yaml),
                "--mcp-only",
                "--no-start-services",
                "--port-offset", str(port_offset),
            ],
        }

        if sandbox_url:
            sandbox_mcp = sandbox_mcp_name(task_id)
            mcp_servers[sandbox_mcp] = {
                "command": _PYTHON,
                "args": [
                    "-m", "ce_runner.mcp_sandbox_tools",
                    "--sandbox-url", sandbox_url,
                    "--task-yaml", task_yaml,
                ],
            }

        # Step 3: Build tools.alsoAllow with serverKey__toolName format
        mock_tool_names = self._extract_mock_tool_names(task)
        allowed = self._build_allowlist(
            mcp_name, mock_tool_names, sandbox_mcp,
        )

        # Step 4: Compute extra deny entries (other MCP servers' tools).
        task_mcp_names = {mcp_name}
        if sandbox_mcp:
            task_mcp_names.add(sandbox_mcp)
        extra_deny = self._build_deny_list(task_mcp_names)

        # Step 5: Single atomic write — MCP servers + agent config
        with open(self.config_path) as f:
            config = json.load(f)

        servers = config.setdefault("mcp", {}).setdefault("servers", {})
        servers.update(mcp_servers)

        agents_list = config.setdefault("agents", {}).setdefault("list", [])
        agents_list[:] = [a for a in agents_list
                          if not a.get("id", "").lower().startswith("claweval-")]
        agents_list.append({
            "id": agent_id,
            "name": agent_id,
            "tools": build_agent_tools(allowed=allowed, extra_deny=extra_deny),
        })
        config.setdefault("agents", {}).setdefault("defaults", {})
        config["agents"]["defaults"]["maxConcurrent"] = max(
            4, config["agents"]["defaults"].get("maxConcurrent", 4)
        )

        atomic_write_config(self.config_path, config)

        return ToolInjectionContext(
            task_id=task_id,
            agent_id=agent_id,
            port_offset=port_offset,
            mcp_name=mcp_name,
            sandbox_url=sandbox_url,
            sandbox_mcp_name=sandbox_mcp,
        )

    def cleanup(self, context: ToolInjectionContext, skip_dirs: bool = False):
        """Clean up injected configuration and generated files.

        Config mutations are done via direct JSON manipulation + atomic write,
        bypassing openclaw's ``writeConfigFile`` size-drop gate.

        Args:
            context: The ToolInjectionContext returned by configure().
            skip_dirs: If True, skip removing agent directories from disk
                (useful for debugging to keep session artifacts).
        """
        try:
            with open(self.config_path) as f:
                config = json.load(f)

            # Remove MCP servers from config
            servers = config.get("mcp", {}).get("servers", {})
            for name in (context.mcp_name, context.sandbox_mcp_name):
                if name and name in servers:
                    del servers[name]

            # Remove agent from config
            agents_list = config.get("agents", {}).get("list", [])
            config.setdefault("agents", {})["list"] = [
                a for a in agents_list
                if a.get("id", "").lower() != context.agent_id.lower()
            ]

            atomic_write_config(self.config_path, config)

            # Clean up generated files
            cleanup_task_skill(context.task_id)

            if not skip_dirs:
                # Remove agent + workspace directories from disk
                agents_root = Path(self.config_path).parent / "agents"
                workspace_root = Path(self.config_path).parent
                for d in (agents_root / context.agent_id.lower(),
                          workspace_root / f"workspace-{context.agent_id.lower()}"):
                    if d.is_dir():
                        import shutil
                        shutil.rmtree(d, ignore_errors=True)

        except Exception:
            pass

    def setup_parallel_workers(
        self,
        task_yamls: list[str],
        parallel: int,
        sandbox_image: str | None = None,
    ) -> dict:
        """Pre-register tools for parallel batch execution.

        Creates one ToolInjectionContext per task and configures the openclaw
        config with all agents and MCP servers. Every task gets a sandbox
        container slot (always-sandbox mode); the container itself is started
        per-trial in batch_runner._execute_one to ensure file-system isolation.

        Args:
            task_yamls: List of task.yaml paths.
            parallel: Number of parallel workers.
            sandbox_image: Docker image for sandbox containers.

        Returns:
            Setup dict with task_slots and parallel count.
        """
        init_config_defaults()
        with open(self.config_path) as f:
            config = json.load(f)

        config.setdefault("mcp", {}).setdefault("servers", {})

        task_slots = {}
        contexts = {}
        mcp_servers: dict[str, dict] = {}

        for idx, task_yaml in enumerate(task_yamls):
            task = load_task_yaml(task_yaml)
            task_id = task["task_id"]
            port_offset = idx * 50  # PORT_STRIDE

            mock_name = mock_mcp_name(task_id)

            mcp_servers[mock_name] = {
                "command": _PYTHON,
                "args": [
                    "-m", "ce_runner.mcp_mock_services",
                    "--task-yaml", os.path.abspath(task_yaml),
                    "--mcp-only",
                    "--no-start-services",
                    "--port-offset", str(port_offset),
                ],
            }

            slot_info = {
                "task_id": task_id,
                "port_offset": port_offset,
                "mock_mcp_name": mock_name,
                "sandbox_mcp_name": None,
                "sandbox_handle": None,
                "sandbox_url": None,
                "agent_id": task_agent_id(task_id),
            }

            # Every task gets a sandbox container slot.
            # Container is NOT started here — it will be created per-trial in
            # _execute_one to ensure file-system isolation between trials.
            from claw_eval.config import SandboxConfig
            from claw_eval.models.task import TaskDefinition
            from claw_eval.runner.sandbox_runner import SandboxRunner

            task_def = TaskDefinition.from_yaml(task_yaml)
            sandbox_cfg = SandboxConfig(image=sandbox_image or "claw-eval-agent:latest")
            runner = SandboxRunner(sandbox_cfg)

            # Compute a deterministic host port for this task slot
            sandbox_host_port = 20000 + idx * 50  # PORT_STRIDE
            sandbox_url = f"http://localhost:{sandbox_host_port}"

            sandbox_mcp = sandbox_mcp_name(task_id)
            mcp_servers[sandbox_mcp] = {
                "command": _PYTHON,
                "args": [
                    "-m", "ce_runner.mcp_sandbox_tools",
                    "--sandbox-url", sandbox_url,
                    "--task-yaml", task_yaml,
                ],
            }
            slot_info["sandbox_mcp_name"] = sandbox_mcp
            slot_info["sandbox_handle"] = None  # deferred to per-trial
            slot_info["sandbox_url"] = sandbox_url
            slot_info["sandbox_host_port"] = sandbox_host_port
            slot_info["sandbox_runner"] = runner
            slot_info["task_def"] = task_def
            slot_info["sandbox_image"] = sandbox_image or "claw-eval-agent:latest"

            task_slots[task_yaml] = slot_info

        # Single atomic config write: MCP servers + agents together
        with open(self.config_path) as f:
            config = json.load(f)

        # Merge MCP servers into config
        servers = config.setdefault("mcp", {}).setdefault("servers", {})
        servers.update(mcp_servers)

        agents_list = config.get("agents", {}).get("list", [])
        agents_list = [a for a in agents_list if not a.get("id", "").lower().startswith("claweval-")]

        # Collect all claw-eval MCP server names for cross-task deny
        all_claw_eval_mcp = set()
        for slot in task_slots.values():
            all_claw_eval_mcp.add(slot["mock_mcp_name"])
            if slot["sandbox_mcp_name"]:
                all_claw_eval_mcp.add(slot["sandbox_mcp_name"])

        # Find pre-existing non-claw-eval MCP servers
        other_mcp_servers = set()
        for server_key in config.get("mcp", {}).get("servers", {}):
            if server_key not in all_claw_eval_mcp:
                other_mcp_servers.add(server_key)

        for task_yaml, slot in task_slots.items():
            agent_id = slot["agent_id"]
            task = load_task_yaml(task_yaml)
            mock_tool_names = self._extract_mock_tool_names(task)
            allowed = self._build_allowlist(
                slot["mock_mcp_name"], mock_tool_names,
                slot["sandbox_mcp_name"],
            )

            # Per-task deny: built-ins + other tasks' MCP + non-claw-eval MCP
            this_task_mcp = {slot["mock_mcp_name"]}
            if slot["sandbox_mcp_name"]:
                this_task_mcp.add(slot["sandbox_mcp_name"])
            extra_deny = []
            for key in (all_claw_eval_mcp - this_task_mcp) | other_mcp_servers:
                extra_deny.append(f"{key}__*")

            agent_entry = {
                "id": agent_id,
                "name": agent_id,
                "tools": build_agent_tools(allowed=allowed, extra_deny=extra_deny),
            }
            agents_list.append(agent_entry)
            slot["agent_id"] = agent_id

        config.setdefault("agents", {})["list"] = agents_list
        config.setdefault("agents", {}).setdefault("defaults", {})
        config["agents"]["defaults"]["maxConcurrent"] = max(
            parallel, config["agents"]["defaults"].get("maxConcurrent", 4)
        )

        atomic_write_config(self.config_path, config)

        return {
            "task_slots": task_slots,
            "parallel": parallel,
            "contexts": contexts,
            "mode": "mcp_server",
        }

    def cleanup_parallel(self, setup_info: dict, skip_dirs: bool = False):
        """Clean up parallel worker configuration.

        Args:
            setup_info: The dict returned by setup_parallel_workers().
            skip_dirs: If True, skip removing agent directories from disk.

        Robustness notes:
        - Each per-task cleanup step is wrapped in its own try/except so that
          a single task's failure does not abort cleanup for the others.
        - Exceptions are logged (not silently swallowed) to aid diagnosis.
        - A final fallback sweep deletes any orphan ``workspace-claweval-*``
          directories and ``mcporter/claw-eval-*.json`` files left behind.
        """
        import shutil
        import traceback

        task_slots = setup_info.get("task_slots", {})

        # Stop sandbox containers
        for task_yaml, slot in task_slots.items():
            handle = slot.get("sandbox_handle")
            runner = slot.get("sandbox_runner")
            if handle and runner:
                try:
                    runner.stop_container(handle)
                except Exception as exc:
                    log(f"[cleanup] stop_container failed for {slot.get('task_id')}: {exc}")

        # Clean config: batch all mutations in memory + single atomic write.
        # This bypasses openclaw's writeConfigFile size-drop gate which rejects
        # writes that shrink the config by >50% (common when removing agents
        # with large tool allow/deny lists).
        try:
            with open(self.config_path) as f:
                config = json.load(f)

            # Remove ce-runner MCP servers (ce-mock-*, ce-sb-*, claw-eval-*)
            servers = config.get("mcp", {}).get("servers", {})
            for key in list(servers.keys()):
                if (key.startswith("ce-mock-") or key.startswith("ce-sb-")
                        or key.startswith("claw-eval-")):
                    del servers[key]

            # Remove all claweval-* agents
            agents_list = config.get("agents", {}).get("list", [])
            config.setdefault("agents", {})["list"] = [
                a for a in agents_list if not a.get("id", "").lower().startswith("claweval-")
            ]

            # Restore tools profile
            config["tools"] = {"profile": "coding"}

            atomic_write_config(self.config_path, config)
        except Exception as exc:
            log(f"[cleanup] config rewrite failed: {exc}\n{traceback.format_exc()}")

        # Per-task cleanup: skills + directories (no CLI calls needed)
        for task_yaml, slot in task_slots.items():
            task_id = slot.get("task_id")
            agent_id = slot.get("agent_id")
            if not task_id or not agent_id:
                continue

            try:
                cleanup_task_skill(task_id)
            except Exception as exc:
                log(f"[cleanup] cleanup_task_skill({task_id}) failed: {exc}")

            if not skip_dirs:
                agents_root = Path(self.config_path).parent / "agents"
                workspace_root = Path(self.config_path).parent
                for d in (agents_root / agent_id.lower(),
                          workspace_root / f"workspace-{agent_id.lower()}"):
                    try:
                        if d.is_dir():
                            shutil.rmtree(d, ignore_errors=True)
                    except Exception as exc:
                        log(f"[cleanup] rmtree {d} failed: {exc}")

        # Fallback sweep: remove any orphan claweval artifacts that earlier
        # runs may have left behind (e.g. crashes, SIGKILL, partial cleanup).
        if not skip_dirs:
            try:
                openclaw_dir = Path(self.config_path).parent
                for d in openclaw_dir.glob("workspace-claweval-*"):
                    if d.is_dir():
                        try:
                            shutil.rmtree(d)
                        except Exception as exc:
                            log(f"[cleanup] orphan sweep: failed to remove {d}: {exc}")
                mcporter_dir = openclaw_dir / "mcporter"
                if mcporter_dir.is_dir():
                    for f in mcporter_dir.glob("claw-eval-*.json"):
                        try:
                            f.unlink(missing_ok=True)
                        except Exception:
                            pass
            except Exception as exc:
                log(f"[cleanup] orphan sweep failed: {exc}")

    @staticmethod
    def _extract_mock_tool_names(task: dict) -> list[str]:
        """Extract tool names from task.yaml ``tools`` field."""
        return [
            t["name"] for t in task.get("tools", [])
            if isinstance(t, dict) and "name" in t
        ]

    @staticmethod
    def _build_allowlist(
        mock_mcp_name: str | None = None,
        mock_tool_names: list[str] | None = None,
        sandbox_mcp_name: str | None = None,
    ) -> list[str]:
        """Build MCP tool entries for ``tools.allow`` in ``serverKey__toolName`` format.

        The openclaw native MCP runtime requires explicit tool names
        in the format ``<serverKey>__<toolName>`` (double underscore).
        Wildcards like ``mcp:server:*`` do NOT work.

        These entries are merged into ``tools.allow`` alongside ``exec``.
        Anti-cheat isolation is achieved via ``tools.deny`` (see caller).
        """
        allowed: list[str] = []
        if mock_mcp_name and mock_tool_names:
            for name in mock_tool_names:
                allowed.append(f"{mock_mcp_name}__{name}")
        if sandbox_mcp_name:
            for name in _SANDBOX_TOOL_NAMES:
                allowed.append(f"{sandbox_mcp_name}__{name}")
        return allowed

    def _build_deny_list(self, task_mcp_names: set[str]) -> list[str]:
        """Build the *extra* deny entries: other MCP servers' tools.

        Returns ``<serverKey>__*`` wildcard entries for every pre-existing MCP
        server that does NOT belong to this task, preventing cross-task or
        cross-server leakage. Built-in gateway tool denies are NOT included
        here — they are added by :func:`build_agent_tools` so the assembly
        stays in one place. The returned list is meant to be passed as
        ``extra_deny`` to :func:`build_agent_tools`.

        Since we cannot enumerate tools without starting the MCP server, we
        deny using the ``<serverKey>__*`` wildcard pattern. If the gateway
        doesn't support wildcards in deny, the non-claw-eval MCP tools will
        still be visible but pose minimal risk (they can't access answers).
        """
        deny: list[str] = []
        try:
            with open(self.config_path) as f:
                config = json.load(f)
            servers = config.get("mcp", {}).get("servers", {})
            for server_key in servers:
                if server_key not in task_mcp_names:
                    deny.append(f"{server_key}__*")
        except Exception:
            pass
        return deny

    @staticmethod
    def _register_mcp_cli(name: str, config: dict):
        """Register MCP server via ``openclaw mcp set`` CLI (persisted)."""
        r = subprocess.run(
            ["openclaw", "mcp", "set", name, json.dumps(config)],
            capture_output=True, text=True, timeout=15,
        )
        if r.returncode != 0:
            log(f"[tool-injector] openclaw mcp set {name} failed: {r.stderr}")

    @staticmethod
    def _unregister_mcp_cli(name: str):
        """Remove MCP server via ``openclaw mcp unset`` CLI."""
        subprocess.run(
            ["openclaw", "mcp", "unset", name],
            capture_output=True, timeout=10,
        )
