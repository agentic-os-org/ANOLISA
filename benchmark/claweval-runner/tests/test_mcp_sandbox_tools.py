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

"""Integration test: Sandbox MCP tool registration and openclaw agent invocation.

Validates the complete pipeline using openclaw's native MCP runtime (stdio):
  1. Register sandbox MCP server via ``openclaw mcp set`` (persisted, not reverted)
  2. Create agent with ``tools.alsoAllow`` using ``serverKey__toolName`` format
  3. Restart gateway for MCP tool discovery
  4. Verify agent can call sandbox tools (Bash, Read, etc.) through MCP
  5. Verify commands execute inside the Docker container (not host)
  6. Cleanup

Key conventions (discovered from custom-mcp-demo-stdio):
  - Native gateway MCP runtime supports stdio transport natively
  - Tool names follow ``<serverKey>__<toolName>`` format
  - Agent must use ``tools.alsoAllow`` (additive list) so MCP tool discovery works
  - No ``openclaw-mcp-adapter`` plugin needed

Prerequisites:
  - openclaw gateway running (port 18789)
  - Docker available (for sandbox container)
  - LLM API configured in config.yaml

Skip conditions:
  - Gateway not healthy → skip all
  - Docker not available → skip container tests
"""

import json
import os
import subprocess
import sys
import time
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))

OPENCLAW_CONFIG = Path.home() / ".openclaw" / "openclaw.json"
GATEWAY_PORT = 18789
TEST_TASK_ID = "M001_clock"
TEST_TASK_YAML = str(REPO_ROOT / "claw-eval" / "tasks" / TEST_TASK_ID / "task.yaml")
AGENT_ID = f"claweval-{TEST_TASK_ID}"
SANDBOX_MCP_NAME = f"claw-eval-sandbox-{TEST_TASK_ID}"

# Tools exposed by mcp_sandbox_tools.py (aligned with claw-eval native sandbox_tools.py)
MCP_TOOL_NAMES = ["Bash", "Read", "Write", "Edit", "Glob", "Grep",
                  "BrowserScreenshot", "ReadMedia", "Download"]

# tools.alsoAllow entries: serverKey__toolName format (native gateway convention)
MCP_ALLOW_LIST = [f"{SANDBOX_MCP_NAME}__{t}" for t in MCP_TOOL_NAMES]

_PYTHON = sys.executable


def _gateway_healthy() -> bool:
    """Check if openclaw gateway is reachable (proxy-safe via curl)."""
    try:
        r = subprocess.run(
            ["curl", "-s", "--noproxy", "127.0.0.1",
             "-o", "/dev/null", "-w", "%{http_code}",
             f"http://127.0.0.1:{GATEWAY_PORT}/health"],
            capture_output=True, text=True, timeout=5,
        )
        return r.stdout.strip() == "200"
    except Exception:
        return False


def _wait_gateway_healthy(timeout: int = 40) -> bool:
    """Wait for gateway to become healthy after restart."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        if _gateway_healthy():
            time.sleep(5)  # extra settle time for MCP discovery
            return True
        time.sleep(1)
    return False


def _restart_gateway() -> bool:
    """Restart gateway and wait for health (proxy-safe)."""
    subprocess.run(
        ["openclaw", "gateway", "restart"],
        capture_output=True, timeout=30,
    )
    return _wait_gateway_healthy()


def _docker_available() -> bool:
    """Check if Docker daemon is reachable."""
    try:
        r = subprocess.run(["docker", "info"], capture_output=True, timeout=10)
        return r.returncode == 0
    except Exception:
        return False


def _register_mcp_via_cli(name: str, sandbox_url: str, task_yaml: str):
    """Register MCP server using ``openclaw mcp set`` CLI (persisted, no revert)."""
    config_json = json.dumps({
        "command": _PYTHON,
        "args": [
            "-m", "ce_runner.mcp_sandbox_tools",
            "--sandbox-url", sandbox_url,
            "--task-yaml", task_yaml,
        ],
    })
    r = subprocess.run(
        ["openclaw", "mcp", "set", name, config_json],
        capture_output=True, text=True, timeout=15,
    )
    assert r.returncode == 0, f"openclaw mcp set failed: {r.stderr}"
    return r


def _unregister_mcp_via_cli(name: str):
    """Remove MCP server via CLI."""
    subprocess.run(
        ["openclaw", "mcp", "unset", name],
        capture_output=True, timeout=10,
    )


def _configure_agent(agent_id: str, allow_list: list[str]):
    """Create/update agent in openclaw.json using the shared build_agent_tools.

    Reuses ``ce_runner.tool_injector.build_agent_tools`` so the test agent gets
    the exact same tools config as production (allow + deny built-ins + exec
    policy), preventing config drift. The agent therefore can only use the MCP
    sandbox tools (which route to the container), not host built-ins.
    """
    from ce_runner.tool_injector import build_agent_tools

    with open(OPENCLAW_CONFIG) as f:
        config = json.load(f)

    agents_list = config.setdefault("agents", {}).setdefault("list", [])
    agents_list[:] = [
        a for a in agents_list
        if a.get("id", "").lower() != agent_id.lower()
    ]
    agents_list.append({
        "id": agent_id,
        "name": agent_id,
        "tools": build_agent_tools(allowed=allow_list),
    })

    with open(OPENCLAW_CONFIG, "w") as f:
        json.dump(config, f, indent=2)


def _remove_agent(agent_id: str):
    """Remove agent from config and delete associated directories."""
    if not OPENCLAW_CONFIG.exists():
        return
    with open(OPENCLAW_CONFIG) as f:
        config = json.load(f)
    agents_list = config.get("agents", {}).get("list", [])
    config["agents"]["list"] = [
        a for a in agents_list
        if a.get("id", "").lower() != agent_id.lower()
    ]
    with open(OPENCLAW_CONFIG, "w") as f:
        json.dump(config, f, indent=2)

    # Delete workspace and agent directories auto-created by gateway
    import shutil
    workspace_dir = Path.home() / ".openclaw" / f"workspace-{agent_id.lower()}"
    agent_dir = Path.home() / ".openclaw" / "agents" / agent_id.lower()
    for d in (workspace_dir, agent_dir):
        if d.is_dir():
            shutil.rmtree(d, ignore_errors=True)


@pytest.fixture(scope="module")
def require_gateway():
    if not _gateway_healthy():
        pytest.skip("openclaw gateway not running on port 18789")


@pytest.fixture(scope="module")
def require_docker():
    if not _docker_available():
        pytest.skip("Docker not available")


# ═══════════════════════════════════════════════════════════════════════════════
# Phase 1: MCP Server Registration (no container needed)
# ═══════════════════════════════════════════════════════════════════════════════

class TestMcpRegistration:
    """Verify MCP server registration via CLI and agent config with tools.alsoAllow."""

    @pytest.fixture(autouse=True)
    def _cleanup(self, require_gateway):
        """Cleanup after test."""
        yield
        _unregister_mcp_via_cli(SANDBOX_MCP_NAME)
        _remove_agent(AGENT_ID)

    def test_register_mcp_via_cli(self):
        """MCP server registered via ``openclaw mcp set`` persists across restarts."""
        _register_mcp_via_cli(
            SANDBOX_MCP_NAME,
            sandbox_url="http://127.0.0.1:20000",
            task_yaml=TEST_TASK_YAML,
        )

        # Verify via CLI list
        r = subprocess.run(
            ["openclaw", "mcp", "list"],
            capture_output=True, text=True, timeout=15,
        )
        assert SANDBOX_MCP_NAME in r.stdout, (
            f"MCP server not listed after registration.\n"
            f"stdout: {r.stdout}\nstderr: {r.stderr}"
        )

        # Verify in config file
        with open(OPENCLAW_CONFIG) as f:
            config = json.load(f)
        servers = config.get("mcp", {}).get("servers", {})
        assert SANDBOX_MCP_NAME in servers, (
            f"MCP server not in config. Servers: {list(servers.keys())}"
        )

    def test_agent_config_uses_tools_also_allow(self):
        """Agent configured via build_agent_tools: allow + deny + exec policy."""
        from ce_runner.tool_injector import _DENY_BUILTIN_TOOLS

        _configure_agent(AGENT_ID, MCP_ALLOW_LIST)

        with open(OPENCLAW_CONFIG) as f:
            config = json.load(f)
        agent = next(
            (a for a in config.get("agents", {}).get("list", [])
             if a.get("id", "").lower() == AGENT_ID.lower()),
            None,
        )
        assert agent is not None, f"Agent {AGENT_ID} not found"

        tools = agent.get("tools", {})
        # Shared build_agent_tools uses allow (not alsoAllow)
        assert "allow" in tools, f"Agent must use tools.allow, got: {tools}"

        allow = tools["allow"]
        # exec must be present to enable MCP tool execution through the bridge
        assert "exec" in allow, "exec must be in tools.allow"

        # MCP tool names must be serverKey__toolName format
        mcp_entries = [e for e in allow if "__" in e]
        assert mcp_entries, f"No MCP tools in allow list: {allow}"
        for entry in mcp_entries:
            server_key, tool_name = entry.split("__", 1)
            assert server_key == SANDBOX_MCP_NAME, (
                f"Server key mismatch: {server_key} != {SANDBOX_MCP_NAME}"
            )

        # Built-in tools must be denied so the agent can only use MCP sandbox
        deny = tools.get("deny", [])
        for builtin in _DENY_BUILTIN_TOOLS:
            assert builtin in deny, f"Built-in '{builtin}' must be in tools.deny"

        # exec policy must be present for MCP bridge execution
        assert tools.get("exec") == {"security": "full", "ask": "off"}, (
            f"Unexpected exec policy: {tools.get('exec')}"
        )

    def test_all_nine_tools_in_allow_list(self):
        """All 9 sandbox tools (aligned with claw-eval native) are included in MCP_ALLOW_LIST."""
        assert len(MCP_TOOL_NAMES) == 9, (
            f"Expected 9 sandbox tools, got {len(MCP_TOOL_NAMES)}: {MCP_TOOL_NAMES}"
        )
        # Verify each tool name maps to correct serverKey__toolName format
        for tool in MCP_TOOL_NAMES:
            expected = f"{SANDBOX_MCP_NAME}__{tool}"
            assert expected in MCP_ALLOW_LIST, (
                f"Missing {expected} in MCP_ALLOW_LIST"
            )
        print(f"[OK] All 9 tools present in allow list: {MCP_TOOL_NAMES}")

    def test_mcp_persists_after_gateway_restart(self):
        """MCP registration persists after gateway restart (not reverted)."""
        _register_mcp_via_cli(
            SANDBOX_MCP_NAME,
            sandbox_url="http://127.0.0.1:20000",
            task_yaml=TEST_TASK_YAML,
        )

        ok = _restart_gateway()
        assert ok, "Gateway restart failed"

        # Verify config NOT reverted
        with open(OPENCLAW_CONFIG) as f:
            config = json.load(f)
        servers = config.get("mcp", {}).get("servers", {})
        assert SANDBOX_MCP_NAME in servers, (
            f"MCP server reverted after restart! Remaining: {list(servers.keys())}"
        )


# ═══════════════════════════════════════════════════════════════════════════════
# Phase 2: Full Pipeline with Live Container
# ═══════════════════════════════════════════════════════════════════════════════

class TestMcpSandboxFullPipeline:
    """End-to-end: container → MCP stdio → gateway → agent → sandbox execution.

    Verifies the agent calls sandbox tools through native MCP (no exec),
    and commands execute inside the Docker container (not host).
    """

    @pytest.fixture(autouse=True)
    def _setup_and_cleanup(self, require_gateway, require_docker):
        """Start container, register MCP, cleanup after."""
        self.container_name = f"claw-agent-test-mcp-{int(time.time())}"
        self.host_port = 20099
        self.sandbox_url = f"http://127.0.0.1:{self.host_port}"
        self.container_id = None

        yield

        # Cleanup container
        if self.container_id:
            subprocess.run(
                ["docker", "rm", "-f", self.container_id],
                capture_output=True, timeout=10,
            )

        # Cleanup MCP + agent
        _unregister_mcp_via_cli(SANDBOX_MCP_NAME)
        _remove_agent(AGENT_ID)

        # Restart gateway to clear MCP state
        _restart_gateway()

    def _start_container(self) -> bool:
        """Start sandbox Docker container and wait for health."""
        r = subprocess.run(
            ["docker", "run", "-d",
             "--name", self.container_name,
             "-p", f"{self.host_port}:8080",
             "claw-eval-agent:latest"],
            capture_output=True, text=True, timeout=30,
        )
        if r.returncode != 0:
            print(f"Container start failed: {r.stderr}")
            return False

        self.container_id = r.stdout.strip()

        deadline = time.time() + 15
        while time.time() < deadline:
            try:
                hc = subprocess.run(
                    ["curl", "-s", "--noproxy", "127.0.0.1",
                     "-o", "/dev/null", "-w", "%{http_code}",
                     f"{self.sandbox_url}/health"],
                    capture_output=True, text=True, timeout=3,
                )
                if hc.stdout.strip() == "200":
                    return True
            except Exception:
                pass
            time.sleep(0.5)
        return False

    def test_full_pipeline_container_mcp_agent(self):
        """Full pipeline: container → MCP → gateway → agent executes in sandbox."""
        # Step A: Start container
        ok = self._start_container()
        assert ok, (
            f"Sandbox container failed to start on {self.sandbox_url}. "
            f"Is 'claw-eval-agent:latest' image available?"
        )
        print(f"[OK] Container started: {self.container_name}")

        # Step B: Verify container responds
        resp = subprocess.run(
            ["curl", "-s", "--noproxy", "127.0.0.1",
             "-X", "POST", "-H", "Content-Type: application/json",
             "-d", '{"command": "echo hello", "timeout_seconds": 5}',
             f"{self.sandbox_url}/exec"],
            capture_output=True, text=True, timeout=10,
        )
        assert resp.returncode == 0
        result = json.loads(resp.stdout)
        assert "hello" in result.get("stdout", "")
        print(f"[OK] Container /exec works")

        # Get container hostname for later verification
        hn_resp = subprocess.run(
            ["curl", "-s", "--noproxy", "127.0.0.1",
             "-X", "POST", "-H", "Content-Type: application/json",
             "-d", '{"command": "hostname", "timeout_seconds": 5}',
             f"{self.sandbox_url}/exec"],
            capture_output=True, text=True, timeout=10,
        )
        container_hostname = json.loads(hn_resp.stdout).get("stdout", "").strip()
        print(f"[OK] Container hostname: {container_hostname}")

        # Step C: Register MCP via CLI + configure agent
        _register_mcp_via_cli(
            SANDBOX_MCP_NAME,
            sandbox_url=self.sandbox_url,
            task_yaml=TEST_TASK_YAML,
        )
        _configure_agent(AGENT_ID, MCP_ALLOW_LIST)
        print("[OK] MCP + agent registered")

        # Step D: Restart gateway for tool discovery
        ok = _restart_gateway()
        assert ok, "Gateway restart failed"
        print("[OK] Gateway restarted")

        # Step E: Agent call — use hostname to verify sandbox execution
        result_file = f"/tmp/test_mcp_agent_{int(time.time())}.json"
        err_file = f"/tmp/test_mcp_agent_{int(time.time())}.err"

        cmd = [
            "openclaw", "agent",
            "--agent", AGENT_ID,
            "--message",
            "Use the Bash tool to run: hostname && echo MCP_PIPELINE_OK. "
            "Return the exact output, nothing else.",
            "--timeout", "60",
            "--json",
        ]

        with open(result_file, "w") as out, open(err_file, "w") as err:
            proc = subprocess.run(cmd, stdout=out, stderr=err, timeout=90)

        agent_output = Path(result_file).read_text() if Path(result_file).exists() else ""
        agent_stderr = Path(err_file).read_text() if Path(err_file).exists() else ""

        print(f"[agent] exit={proc.returncode}")
        print(f"[agent] stdout: {agent_output[:500]}")
        print(f"[agent] stderr: {agent_stderr[:300]}")

        # Cleanup temp files
        for f in (result_file, err_file):
            try:
                os.remove(f)
            except OSError:
                pass

        # Verify: agent must NOT have tried exec
        assert "Tool exec not found" not in agent_output, (
            "Agent tried built-in exec — MCP tools not injected!"
        )
        assert "Tool exec not found" not in agent_stderr, (
            "Agent stderr shows exec not found — MCP tools not injected!"
        )

        # Verify: output contains marker
        assert "MCP_PIPELINE_OK" in agent_output, (
            f"Agent did not produce expected output.\n"
            f"Output: {agent_output[:500]}"
        )

        # Verify: hostname matches container (proves sandbox execution)
        if container_hostname:
            assert container_hostname in agent_output, (
                f"Agent output does not contain container hostname "
                f"({container_hostname}), suggesting command did NOT run "
                f"in the sandbox container.\nOutput: {agent_output[:500]}"
            )
            print(f"[OK] Agent executed command in sandbox container "
                  f"(hostname={container_hostname})")
        else:
            print("[WARN] Could not verify sandbox execution (no container hostname)")

        print("[OK] Full pipeline passed — MCP sandbox tools work without exec")


# ═══════════════════════════════════════════════════════════════════════════════
# Phase 3: Diagnostic Checks (can run independently)
# ═══════════════════════════════════════════════════════════════════════════════

class TestMcpToolDiscoveryDiagnostic:
    """Diagnostic tests to pinpoint MCP tool injection failures.

    Run individually:
      pytest tests/test_mcp_sandbox_tools.py::TestMcpToolDiscoveryDiagnostic -v
    """

    def test_diag_mcp_sandbox_tools_module_importable(self):
        """Diagnostic: ce_runner.mcp_sandbox_tools module can be imported."""
        import ce_runner.mcp_sandbox_tools  # noqa: F401

    def test_diag_mcp_sandbox_tools_starts_without_container(self):
        """Diagnostic: MCP sandbox tools process starts (even without container).

        The process should start and stay alive for stdin/stdout MCP transport,
        even if the sandbox URL is unreachable. Gateway starts the process and
        queries tools; tool *calls* will fail but *discovery* should succeed.
        """
        proc = subprocess.Popen(
            [_PYTHON, "-m", "ce_runner.mcp_sandbox_tools",
             "--sandbox-url", "http://localhost:99999",
             "--task-yaml", TEST_TASK_YAML],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        time.sleep(2)

        poll = proc.poll()
        if poll is not None:
            stderr = proc.stderr.read().decode()
            pytest.fail(
                f"MCP sandbox process exited immediately (code={poll}). "
                f"stderr: {stderr[:500]}"
            )

        proc.terminate()
        proc.wait(timeout=5)
        print("[OK] mcp_sandbox_tools process starts and stays alive")

    def test_diag_tool_naming_convention(self):
        """Diagnostic: verify tool naming follows serverKey__toolName convention."""
        for tool in MCP_TOOL_NAMES:
            expected = f"{SANDBOX_MCP_NAME}__{tool}"
            assert expected in MCP_ALLOW_LIST, (
                f"Expected {expected} in allow list"
            )
        print(f"[OK] Tool naming: {MCP_ALLOW_LIST}")

    def test_diag_path_map_has_nine_tools(self):
        """Diagnostic: mcp_sandbox_tools._PATH_MAP has 9 tools (aligned with claw-eval native)."""
        import ce_runner.mcp_sandbox_tools as mst
        assert len(mst._PATH_MAP) == 9, (
            f"Expected 9 tools in _PATH_MAP, got {len(mst._PATH_MAP)}: "
            f"{list(mst._PATH_MAP.keys())}"
        )
        # Verify all expected tools are present
        for tool in MCP_TOOL_NAMES:
            assert tool in mst._PATH_MAP, f"Missing {tool} in _PATH_MAP"
        print(f"[OK] _PATH_MAP has 9 tools: {list(mst._PATH_MAP.keys())}")

    def test_diag_new_tool_params_roundtrip(self):
        """Diagnostic: parameter translation works for BrowserScreenshot, ReadMedia, Download."""
        import ce_runner.mcp_sandbox_tools as mst

        # BrowserScreenshot: pass-through (no translation needed)
        bs_params = {"url": "http://example.com", "wait_seconds": 3.0, "frame_count": 6}
        bs_result = mst._translate_payload("BrowserScreenshot", dict(bs_params))
        assert bs_result == bs_params, f"BrowserScreenshot params changed: {bs_result}"

        # ReadMedia: file_path → path translation
        rm_params = {"file_path": "/workspace/video.mp4", "max_frames": 5, "fps": 2.0}
        rm_result = mst._translate_payload("ReadMedia", dict(rm_params))
        assert rm_result["path"] == "/workspace/video.mp4", f"ReadMedia path: {rm_result}"
        assert "file_path" not in rm_result, "file_path not removed"
        assert rm_result["max_frames"] == 5
        assert rm_result["fps"] == 2.0

        # ReadMedia: direct path (no file_path) passes through unchanged
        rm_params2 = {"path": "/workspace/img.png", "media_type": "image"}
        rm_result2 = mst._translate_payload("ReadMedia", dict(rm_params2))
        assert rm_result2 == rm_params2, f"ReadMedia params changed: {rm_result2}"

        # Download: file_path → path translation
        dl_params = {"file_path": "/workspace/output.html", "max_bytes": 1000}
        dl_result = mst._translate_payload("Download", dict(dl_params))
        assert dl_result["path"] == "/workspace/output.html", f"Download path: {dl_result}"
        assert "file_path" not in dl_result, "file_path not removed"
        assert dl_result["max_bytes"] == 1000

        print("[OK] New tool parameter translation works correctly")

    def test_diag_config_format(self, require_gateway):
        """Diagnostic: verify current config follows correct conventions."""
        with open(OPENCLAW_CONFIG) as f:
            config = json.load(f)

        # Check MCP servers
        servers = config.get("mcp", {}).get("servers", {})
        print(f"MCP servers: {list(servers.keys())}")

        # Check agents
        agents = config.get("agents", {}).get("list", [])
        for a in agents:
            aid = a.get("id", "")
            tools = a.get("tools", {})
            if "allow" in tools:
                print(f"  [WARN] Agent '{aid}' uses allow (should use alsoAllow)")
            if "alsoAllow" in tools:
                also_allow = tools["alsoAllow"]
                mcp_tools = [t for t in also_allow if "__" in t]
                print(f"  Agent '{aid}': {len(mcp_tools)} MCP tools, "
                      f"{len(also_allow) - len(mcp_tools)} other tools")


# ═══════════════════════════════════════════════════════════════════════════════
# Phase 4: ReadMedia frame-window truncation handling (pure unit tests)
# ═══════════════════════════════════════════════════════════════════════════════

class TestReadMediaFrameWindow:
    """Unit tests for the ReadMedia frame-window truncation helpers.

    Reproduces the M067 failure: agent requests start=0/end=60/fps=2/max_frames=30
    but the container silently returns only 0.0-14.5s (30 frames) with no hint
    that the window was truncated.
    """

    @staticmethod
    def _truncated_response(last_ts: float = 14.5, n_frames: int = 30) -> str:
        """Container response that stopped early after hitting max_frames."""
        step = last_ts / (n_frames - 1) if n_frames > 1 else 0.0
        frames = [
            {
                "index": i,
                "timestamp_s": round(i * step, 3),
                "mime_type": "image/png",
                "image_b64": "ZmFrZQ==",
            }
            for i in range(n_frames)
        ]
        return json.dumps({"media_type": "video", "frames": frames})

    def test_coverage_warning_when_truncated(self):
        """end_time=60 but last frame at 14.5s → truncation warning emitted."""
        import ce_runner.mcp_sandbox_tools as mst

        raw = self._truncated_response(last_ts=14.5, n_frames=30)
        warning = mst._coverage_warning(
            raw, requested_end_time=60.0, max_frames=30, fps=2.0)
        assert warning is not None, "Expected a truncation warning"
        assert "WARNING" in warning
        assert "14.5" in warning
        assert "60" in warning

    def test_no_warning_when_window_covered(self):
        """Last frame close to end_time → no warning."""
        import ce_runner.mcp_sandbox_tools as mst

        raw = self._truncated_response(last_ts=59.5, n_frames=30)
        warning = mst._coverage_warning(
            raw, requested_end_time=60.0, max_frames=30, fps=2.0)
        assert warning is None, f"Did not expect a warning, got: {warning}"

    def test_no_warning_when_end_time_none(self):
        """No end_time given → detection is skipped entirely."""
        import ce_runner.mcp_sandbox_tools as mst

        raw = self._truncated_response(last_ts=14.5, n_frames=30)
        assert mst._coverage_warning(
            raw, requested_end_time=None, max_frames=30, fps=2.0) is None

    def test_no_warning_on_error_response(self):
        """Error / non-frame responses don't produce a warning."""
        import ce_runner.mcp_sandbox_tools as mst

        raw = json.dumps({"error": "decode failed"})
        assert mst._coverage_warning(
            raw, requested_end_time=60.0, max_frames=30, fps=2.0) is None

    def test_align_max_frames_raises_to_cover_window(self):
        """end_time=60, fps=2 needs 120 frames → max_frames bumped to 120 cap."""
        import ce_runner.mcp_sandbox_tools as mst

        aligned = mst._align_max_frames(
            max_frames=30, fps=2.0, start_time=0.0, end_time=60.0)
        assert aligned == 120

    def test_align_max_frames_respects_safe_cap(self):
        """A huge window never exceeds the 120-frame safety cap."""
        import ce_runner.mcp_sandbox_tools as mst

        aligned = mst._align_max_frames(
            max_frames=8, fps=5.0, start_time=0.0, end_time=600.0)
        assert aligned == 120

    def test_align_max_frames_smaller_window(self):
        """A modest window gets exactly the frames it needs, under the cap."""
        import ce_runner.mcp_sandbox_tools as mst

        aligned = mst._align_max_frames(
            max_frames=8, fps=2.0, start_time=0.0, end_time=20.0)
        assert aligned == 40

    def test_align_max_frames_unchanged_when_fits(self):
        """When max_frames already covers the window it is left untouched."""
        import ce_runner.mcp_sandbox_tools as mst

        aligned = mst._align_max_frames(
            max_frames=30, fps=1.0, start_time=0.0, end_time=10.0)
        assert aligned == 30

    def test_align_max_frames_no_end_time(self):
        """No end_time → max_frames is returned unchanged."""
        import ce_runner.mcp_sandbox_tools as mst

        aligned = mst._align_max_frames(
            max_frames=8, fps=2.0, start_time=0.0, end_time=None)
        assert aligned == 8

    def test_warning_embedded_in_summary_textcontent(self):
        """Warning text is prepended to the media summary TextContent block."""
        import ce_runner.mcp_sandbox_tools as mst
        from mcp.types import TextContent

        raw = self._truncated_response(last_ts=14.5, n_frames=30)
        warning = mst._coverage_warning(
            raw, requested_end_time=60.0, max_frames=30, fps=2.0)
        media = mst._build_media_result(raw)
        assert media is not None
        assert isinstance(media[-1], TextContent)

        # Simulate read_media's injection of the warning into the summary block.
        media[-1] = TextContent(
            type="text", text=f"{warning}\n\n{media[-1].text}")
        assert "WARNING" in media[-1].text
        assert "frame_count" in media[-1].text  # original summary preserved
