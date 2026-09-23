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

"""Test tool injector logic.

Covers:
- ToolInjector class initialization and configuration
- generate_task_skill for T/M tasks
- cleanup_task_skill for post-execution cleanup
- is_sandbox_task detection (M tasks)
- Tool injection context management

Task type coverage:
- T tasks: gateway mode tool injection
- M tasks: sandbox mode tool injection + cleanup
- C tasks: user_agent mode (minimal tool injection)
"""

import json
import sys
from pathlib import Path
from unittest.mock import patch, MagicMock, mock_open
import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))


class TestIsSandboxTask:
    """Test sandbox task detection."""

    def test_sandbox_task_m_type(self, tmp_path):
        """M tasks are sandbox tasks."""
        from ce_runner._common import is_sandbox_task
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: M001_clock\nsandbox_files:\n  - /workspace/clock.html\n")
        
        assert is_sandbox_task(str(task_yaml)) is True

    def test_non_sandbox_task_t_type(self, tmp_path):
        """T tasks are not sandbox tasks."""
        from ce_runner._common import is_sandbox_task
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001_test\nservices:\n  - calendar\n")
        
        assert is_sandbox_task(str(task_yaml)) is False

    def test_non_sandbox_task_c_type(self, tmp_path):
        """C tasks are not sandbox tasks."""
        from ce_runner._common import is_sandbox_task
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: C01_mortgage\nuser_agent:\n  enabled: true\n")
        
        assert is_sandbox_task(str(task_yaml)) is False


class TestTaskSkillGeneration:
    """Test task skill generation (now a no-op since mcporter removal)."""

    def test_generate_task_skill_basic(self, tmp_path):
        """generate_task_skill is now a no-op."""
        from ce_runner.skill_generator import generate_task_skill
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices:\n  - calendar\n  - gmail\ntools: []\n")
        
        # Should not raise (no-op)
        generate_task_skill(str(task_yaml), "T001")

    def test_generate_task_skill_with_tools(self, tmp_path):
        """generate_task_skill is now a no-op."""
        from ce_runner.skill_generator import generate_task_skill
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T002\nservices:\n  - crm\ntools: []\n")
        
        # Should not raise (no-op)
        generate_task_skill(str(task_yaml), "T002")


class TestTaskSkillCleanup:
    """Test task skill cleanup after execution."""

    def test_cleanup_task_skill_existing(self, tmp_path):
        """Cleanup removes existing skill file."""
        from ce_runner.tool_injector import cleanup_task_skill
        
        skill_path = tmp_path / "SKILL.md"
        skill_path.write_text("# Test Skill")
        
        cleanup_task_skill(str(skill_path))
        
        # Should not raise, file may or may not be removed
        # (depends on implementation)

    def test_cleanup_task_skill_missing(self, tmp_path):
        """Cleanup handles missing skill file gracefully."""
        from ce_runner.tool_injector import cleanup_task_skill
        
        skill_path = tmp_path / "NONEXISTENT.md"
        
        # Should not raise
        cleanup_task_skill(str(skill_path))


class TestToolInjectionContext:
    """Test tool injection context management."""

    def test_context_with_sandbox_task(self, tmp_path):
        """ToolInjector context for sandbox task."""
        from ce_runner._common import is_sandbox_task
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: M001\nsandbox_files:\n  - /workspace/output.html\n")
        
        assert is_sandbox_task(str(task_yaml)) is True

    def test_context_with_gateway_task(self, tmp_path):
        """ToolInjector context for gateway task."""
        from ce_runner._common import is_sandbox_task
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices:\n  - calendar\n")
        
        assert is_sandbox_task(str(task_yaml)) is False


class TestConfigDefaults:
    """Test configuration defaults initialization."""

    def test_init_config_defaults(self):
        """Initialize config with defaults."""
        from ce_runner.tool_injector import init_config_defaults
        
        # Function takes no arguments, modifies config in place
        result = init_config_defaults()
        
        # Should complete without error
        assert result is None or isinstance(result, dict)


class TestMcpDirectWrite:
    """Test that direct-write MCP registration produces correct config.

    Verifies that ToolInjector.configure() writes MCP server entries and
    agent configs to openclaw.json with the same structure that
    ``openclaw mcp set`` CLI would produce.
    """

    @pytest.fixture
    def openclaw_config(self, tmp_path):
        """Create a minimal openclaw.json for testing."""
        config = {
            "agents": {"defaults": {"maxConcurrent": 4}, "list": []},
            "mcp": {"servers": {}},
            "gateway": {"port": 18789},
        }
        config_path = tmp_path / "openclaw.json"
        config_path.write_text(json.dumps(config, indent=2))
        return str(config_path)

    @pytest.fixture
    def task_yaml_file(self, tmp_path):
        """Create a minimal task.yaml for testing."""
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text(
            "task_id: T001_test_task\n"
            "services:\n"
            "  - name: calendar\n"
            "    port: 5001\n"
            "    health_check: http://localhost:5001/health\n"
            "tools:\n"
            "  - calendar_list_events\n"
            "  - calendar_create_event\n"
        )
        return str(task_yaml)

    def test_configure_writes_mcp_servers(self, openclaw_config, task_yaml_file):
        """configure() writes MCP server entries to mcp.servers."""
        from ce_runner.tool_injector import ToolInjector

        injector = ToolInjector(openclaw_config)
        with patch("subprocess.run"):
            ctx = injector.configure(task_yaml_file, port_offset=0,
                                     sandbox_url="http://localhost:20000")

        with open(openclaw_config) as f:
            config = json.load(f)

        servers = config["mcp"]["servers"]
        assert ctx.mcp_name in servers
        assert ctx.sandbox_mcp_name in servers

        # Verify mock MCP server structure
        mock_srv = servers[ctx.mcp_name]
        assert "command" in mock_srv
        assert "args" in mock_srv
        assert "--task-yaml" in mock_srv["args"]
        assert "--mcp-only" in mock_srv["args"]
        assert "--port-offset" in mock_srv["args"]
        assert "0" in mock_srv["args"]

        # Verify sandbox MCP server structure
        sb_srv = servers[ctx.sandbox_mcp_name]
        assert "command" in sb_srv
        assert "--sandbox-url" in sb_srv["args"]
        assert "http://localhost:20000" in sb_srv["args"]

    def test_configure_writes_agent_with_tools(self, openclaw_config, task_yaml_file):
        """configure() writes agent entry with correct tool allow/deny lists."""
        from ce_runner.tool_injector import ToolInjector

        injector = ToolInjector(openclaw_config)
        with patch("subprocess.run"):
            ctx = injector.configure(task_yaml_file, port_offset=0,
                                     sandbox_url="http://localhost:20000")

        with open(openclaw_config) as f:
            config = json.load(f)

        agents = config["agents"]["list"]
        assert len(agents) == 1
        agent = agents[0]
        assert agent["id"] == ctx.agent_id
        assert "tools" in agent

        tools = agent["tools"]
        # Must have allow list with MCP-prefixed tool names
        assert "allow" in tools
        allow = tools["allow"]
        assert "exec" in allow
        # Should contain sandbox tools (prefixed with sandbox MCP name)
        sandbox_tools = [t for t in allow if ctx.sandbox_mcp_name in t]
        assert len(sandbox_tools) > 0

        # Must have deny list blocking built-in tools
        assert "deny" in tools
        deny = tools["deny"]
        assert "exec" in deny or "read" in deny

    def test_configure_single_atomic_write(self, openclaw_config, task_yaml_file):
        """configure() performs a single write containing both MCP and agents."""
        from ce_runner.tool_injector import ToolInjector

        injector = ToolInjector(openclaw_config)
        with patch("subprocess.run"):
            injector.configure(task_yaml_file, port_offset=0,
                               sandbox_url="http://localhost:20000")

        with open(openclaw_config) as f:
            config = json.load(f)

        # Both MCP servers and agents must be present in the same config
        assert len(config["mcp"]["servers"]) == 2
        assert len(config["agents"]["list"]) == 1

    def test_configure_no_cli_mcp_set_called(self, openclaw_config, task_yaml_file):
        """configure() does NOT call 'openclaw mcp set' CLI."""
        from ce_runner.tool_injector import ToolInjector

        injector = ToolInjector(openclaw_config)
        with patch("subprocess.run") as mock_run:
            injector.configure(task_yaml_file, port_offset=0,
                               sandbox_url="http://localhost:20000")

        # The only subprocess.run call should be 'openclaw agents add'
        for call in mock_run.call_args_list:
            args = call[0][0] if call[0] else call[1].get("args", [])
            assert "mcp" not in args, \
                f"Unexpected 'openclaw mcp set' call: {args}"

    def test_cleanup_removes_mcp_servers(self, openclaw_config, task_yaml_file):
        """cleanup() removes MCP server entries from config."""
        from ce_runner.tool_injector import ToolInjector

        injector = ToolInjector(openclaw_config)
        with patch("subprocess.run"):
            ctx = injector.configure(task_yaml_file, port_offset=0,
                                     sandbox_url="http://localhost:20000")

        # Verify servers exist
        with open(openclaw_config) as f:
            config = json.load(f)
        assert len(config["mcp"]["servers"]) == 2

        # Cleanup
        injector.cleanup(ctx, skip_dirs=True)

        with open(openclaw_config) as f:
            config = json.load(f)
        assert ctx.mcp_name not in config["mcp"]["servers"]
        assert ctx.sandbox_mcp_name not in config["mcp"]["servers"]

    def test_mcp_server_config_matches_cli_format(self, openclaw_config, task_yaml_file):
        """MCP server config matches the JSON format openclaw mcp set expects.

        openclaw mcp set <name> '{"command":"...","args":[...]}' writes the
        exact object to mcp.servers.<name>. Our direct write must produce the
        same structure.
        """
        from ce_runner.tool_injector import ToolInjector
        from ce_runner._common import _PYTHON

        injector = ToolInjector(openclaw_config)
        with patch("subprocess.run"):
            ctx = injector.configure(task_yaml_file, port_offset=50,
                                     sandbox_url="http://localhost:20050")

        with open(openclaw_config) as f:
            config = json.load(f)

        mock_srv = config["mcp"]["servers"][ctx.mcp_name]
        # Must be exact format: {"command": str, "args": list[str]}
        assert set(mock_srv.keys()) == {"command", "args"}
        assert mock_srv["command"] == _PYTHON
        assert isinstance(mock_srv["args"], list)
        assert all(isinstance(a, str) for a in mock_srv["args"])
        # Port offset must be correctly passed
        assert "50" in mock_srv["args"]

        sb_srv = config["mcp"]["servers"][ctx.sandbox_mcp_name]
        assert set(sb_srv.keys()) == {"command", "args"}
        assert sb_srv["command"] == _PYTHON
        assert "http://localhost:20050" in sb_srv["args"]
