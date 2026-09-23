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

"""Test that cleanup_config fully removes agent artifacts."""

import json
import os
import subprocess
import time
from pathlib import Path

import pytest

# Repo root is one level up from this file
REPO_ROOT = Path(__file__).resolve().parent.parent
TEST_TASK = "T001zh_email_triage"
TASK_YAML = str(REPO_ROOT / "claw-eval" / "tasks" / TEST_TASK / "task.yaml")
OPENCLAW_CONFIG = str(Path.home() / ".openclaw" / "openclaw.json")


@pytest.fixture(autouse=True)
def setup():
    """Check prerequisites."""
    assert Path(TASK_YAML).exists(), f"Task YAML not found: {TASK_YAML}"
    assert Path(OPENCLAW_CONFIG).exists(), f"OpenClaw config not found: {OPENCLAW_CONFIG}"


def _agent_dirs(task_id: str) -> tuple[Path, Path]:
    """Return (workspace_dir, agent_dir) paths for a task."""
    workspace = Path.home() / ".openclaw" / f"workspace-claweval-{task_id.lower()}"
    agent_dir = Path.home() / ".openclaw" / "agents" / f"claweval-{task_id.lower()}"
    return workspace, agent_dir


def _mcporter_config(task_id: str) -> Path:
    return Path.home() / ".openclaw" / "mcporter" / f"claw-eval-{task_id.lower()}.json"


def _config_agents() -> list[str]:
    """Return list of agent IDs from openclaw.json."""
    with open(OPENCLAW_CONFIG) as f:
        config = json.load(f)
    return [a.get("id", "") for a in config.get("agents", {}).get("list", [])]


def _cli_agents() -> list[str]:
    """Return list of agent IDs from CLI."""
    result = subprocess.run(
        ["openclaw", "agents", "list"], capture_output=True, text=True, timeout=10
    )
    ids = []
    for line in result.stdout.split("\n"):
        line = line.strip()
        if line.startswith("- "):
            aid = line[2:].split(" ")[0]
            ids.append(aid)
    return ids


class TestCleanupConfig:
    """Verify cleanup_config fully removes agent artifacts."""

    def test_cleanup_removes_all_artifacts(self):
        """After cleanup, no agent artifacts should remain."""
        import sys
        sys.path.insert(0, str(REPO_ROOT / "src"))
        from ce_runner.infra import configure_tools, cleanup_config

        # Create agent
        configure_tools(TASK_YAML)
        time.sleep(1)

        # Verify agent exists
        agents_before = _config_agents()
        assert any("claweval" in a.lower() for a in agents_before), \
            f"Agent not found after configure_tools: {agents_before}"

        workspace, agent_dir = _agent_dirs(TEST_TASK)
        assert workspace.exists(), f"Workspace not created: {workspace}"
        assert agent_dir.exists(), f"Agent dir not created: {agent_dir}"

        # Run cleanup
        cleanup_config()
        time.sleep(1)

        # Verify all artifacts removed
        agents_after = _config_agents()
        assert not any("claweval" in a.lower() for a in agents_after), \
            f"Agent still in config after cleanup: {agents_after}"

        assert not workspace.exists(), f"Workspace not removed: {workspace}"
        assert not agent_dir.exists(), f"Agent dir not removed: {agent_dir}"

    def test_cleanup_tools_config_restored(self):
        """After configure, agent uses tools.allow (MCP serverKey__toolName) plus tools.deny (blocking built-in tools); after cleanup the agent is removed."""
        import sys
        sys.path.insert(0, str(REPO_ROOT / "src"))
        from ce_runner.infra import configure_tools, cleanup_config
        from ce_runner.tool_injector import _DENY_BUILTIN_TOOLS

        # Create agent
        configure_tools(TASK_YAML)
        time.sleep(1)

        # Verify agent has tools.alsoAllow (per-agent, not global)
        with open(OPENCLAW_CONFIG) as f:
            config = json.load(f)
        agents = config.get("agents", {}).get("list", [])
        claweval_agents = [a for a in agents if a.get("id", "").startswith("claweval-")]
        assert len(claweval_agents) > 0, "No claweval agent found after configure"
        agent_tools = claweval_agents[0].get("tools", {})
        # Must use tools.allow with serverKey__toolName format (merged)
        allow_list = agent_tools.get("allow", [])
        mcp_tools = [t for t in allow_list if "__" in t]
        assert len(mcp_tools) > 0, f"Agent must have MCP tools in allow, got: {allow_list}"
        # Must deny built-in tools for anti-cheat isolation
        assert "deny" in agent_tools, f"Agent must have tools.deny, got: {agent_tools}"
        assert set(_DENY_BUILTIN_TOOLS).issubset(set(agent_tools["deny"])), \
            f"tools.deny must block all built-in tools {_DENY_BUILTIN_TOOLS}, got: {agent_tools['deny']}"

        # Run cleanup
        cleanup_config()

        # Verify agent removed
        with open(OPENCLAW_CONFIG) as f:
            config = json.load(f)
        agents = config.get("agents", {}).get("list", [])
        claweval_agents = [a for a in agents if a.get("id", "").startswith("claweval-")]
        assert len(claweval_agents) == 0, f"claweval agent not removed: {claweval_agents}"

    def test_cleanup_mcp_servers_removed(self):
        """After cleanup, mcp.servers should not contain claw-eval entries."""
        import sys
        sys.path.insert(0, str(REPO_ROOT / "src"))
        from ce_runner.infra import configure_tools, cleanup_config

        # Create agent
        configure_tools(TASK_YAML)
        time.sleep(1)

        # Run cleanup
        cleanup_config()

        # Verify mcp.servers cleaned (no claw-eval entries)
        with open(OPENCLAW_CONFIG) as f:
            config = json.load(f)
        servers = config.get("mcp", {}).get("servers", {})
        assert "claw-eval-mock" not in servers
        assert "claw-eval-sandbox" not in servers
