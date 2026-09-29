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

"""Environment readiness tests — verify the current env can run ce-runner.

Usage: pytest tests/test_environment.py -v

Each test corresponds to a prerequisite; failure messages include remediation commands.
Groups:
  Required — missing means no task can run
  Runtime  — must be ready at runtime (gateway, ports, docker daemon)
  Sandbox  — only needed for --sandbox mode
  Optional — missing only affects some features (mcporter, etc.)
"""

from __future__ import annotations

import importlib
import json
import os
import re
import shutil
import socket
import subprocess
import sys
from pathlib import Path

import pytest
import yaml

REPO_ROOT = Path(__file__).resolve().parent.parent
CLAW_EVAL_DIR = REPO_ROOT / "claw-eval"
OPENCLAW_CONFIG = Path.home() / ".openclaw" / "openclaw.json"
REQUIRED_OPENCLAW = (2026, 4, 22)
SANDBOX_IMAGE = "claw-eval-agent:latest"
MOCK_PORTS = list(range(9100, 9117))  # 9100-9116

# Runtime-critical imports (missing = task execution crashes immediately)
# (module_name, source, fix hint)
REQUIRED_MODULES = [
    ("yaml",        "ce-runner core dep",       "pip install -e ."),
    ("httpx",       "ce-runner core dep",       "pip install -e ."),
    ("fastapi",     "ce-runner core dep",       "pip install -e ."),
    ("uvicorn",     "ce-runner core dep",       "pip install -e ."),
    ("pypdf",       "ce-runner core dep",       "pip install -e ."),
    ("trafilatura", "ce-runner core dep",       "pip install -e ."),
    ("docker",      "ce-runner core dep",       "pip install -e ."),
    ("openai",      "ce-runner core dep",       "pip install -e ."),
    ("mcp",         "ce-runner core dep",       "pip install -e ."),
    ("pydantic",    "claw-eval core dep",       "pip install -e claw-eval[mock,sandbox]"),
]


def _port_free(port: int) -> bool:
    """Check if a port can be bound (i.e. not occupied)."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            s.bind(("127.0.0.1", port))
            return True
        except OSError:
            return False


# ────────────────────────────────────────────────────────────────────────────
# Required — cannot run any task without these
# ────────────────────────────────────────────────────────────────────────────

class TestRequired:
    """Fundamental prerequisites for running ce-runner."""

    def test_python_version(self):
        assert sys.version_info >= (3, 11), (
            f"Python version too low: {sys.version_info[:2]}, requires >= 3.11"
        )

    def test_submodule_initialized(self):
        pyproject = CLAW_EVAL_DIR / "pyproject.toml"
        assert pyproject.exists(), (
            f"claw-eval submodule not initialized: {pyproject} does not exist."
            f" Fix: git -C {REPO_ROOT} submodule update --init --recursive"
        )

    def test_ce_runner_cli_installed(self):
        assert shutil.which("ce-runner"), (
            "ce-runner CLI not installed. Fix: pip install -e ."
        )

    def test_ce_runner_importable(self):
        try:
            import ce_runner  # noqa: F401
        except ImportError as e:
            pytest.fail(f"Cannot import ce_runner: {e}. Fix: pip install -e .")

    def test_claw_eval_importable(self):
        try:
            import claw_eval  # noqa: F401
        except ImportError as e:
            pytest.fail(
                f"Cannot import claw_eval: {e}."
                f" Fix: pip install -e claw-eval[mock,sandbox]"
            )

    @pytest.mark.parametrize("module,source,fix", REQUIRED_MODULES,
                             ids=[m[0] for m in REQUIRED_MODULES])
    def test_required_module_importable(self, module, source, fix):
        """Runtime-critical dependencies (mock services / judge / config loading)."""
        try:
            importlib.import_module(module)
        except ImportError as e:
            pytest.fail(
                f"Missing dependency {module!r} ({source}): {e}. Fix: {fix}"
            )

    def test_openclaw_cli_installed(self):
        assert shutil.which("openclaw"), (
            "openclaw CLI not installed (cannot be obtained via pip; install manually per official docs)"
        )

    def test_openclaw_version(self):
        if not shutil.which("openclaw"):
            pytest.skip("openclaw not installed")
        result = subprocess.run(
            ["openclaw", "--version"], capture_output=True, text=True, timeout=10
        )
        match = re.search(r"(\d+)\.(\d+)\.(\d+)", result.stdout)
        assert match, f"Cannot parse openclaw version: {result.stdout!r}"
        version = tuple(int(x) for x in match.groups())
        assert version >= REQUIRED_OPENCLAW, (
            f"openclaw version too low: {'.'.join(map(str, version))},"
            f" requires >= {'.'.join(map(str, REQUIRED_OPENCLAW))}"
        )

    def test_openclaw_config_exists(self):
        assert OPENCLAW_CONFIG.exists(), (
            f"{OPENCLAW_CONFIG} does not exist."
            f" Fix: run openclaw once to generate config, then run"
            f" python scripts/configure_openclaw.py"
        )

    def test_openclaw_config_well_formed(self):
        if not OPENCLAW_CONFIG.exists():
            pytest.skip("openclaw config does not exist")
        with open(OPENCLAW_CONFIG) as f:
            config = json.load(f)
        gateway = config.get("gateway", {})
        assert "port" in gateway, "openclaw.json missing gateway.port"
        assert gateway.get("auth", {}).get("token"), (
            "openclaw.json missing gateway.auth.token"
        )
        # ce-runner hard dependency: fields set by configure_openclaw.py
        endpoints = gateway.get("http", {}).get("endpoints", {})
        cc = endpoints.get("chatCompletions", {})
        assert cc.get("enabled") is True, (
            "gateway.http.endpoints.chatCompletions.enabled is not set."
            " Fix: python scripts/configure_openclaw.py"
        )

    def test_openclaw_config_size_reasonable(self):
        """openclaw.json should not be bloated by residual MCP entries (normally < 20KB)."""
        if not OPENCLAW_CONFIG.exists():
            pytest.skip("openclaw config does not exist")
        size_kb = OPENCLAW_CONFIG.stat().st_size / 1024
        assert size_kb < 50, (
            f"openclaw.json abnormally large: {size_kb:.0f}KB (normally < 20KB),"
            f" possibly due to residual MCP server entries."
            f" Fix: python scripts/check_openclaw_env.py --fix"
        )

    def test_openclaw_backup_not_bloated(self):
        """Backup files should not contain residual entries; otherwise gateway size-drop guard rolls back cleanup."""
        if not OPENCLAW_CONFIG.exists():
            pytest.skip("openclaw config does not exist")
        from pathlib import Path
        bak_files = list(Path(OPENCLAW_CONFIG).parent.glob("openclaw.json.bak*"))
        last_good = Path(OPENCLAW_CONFIG).parent / "openclaw.json.last-good"
        if last_good.exists():
            bak_files.append(last_good)

        bloated = []
        for bak in bak_files:
            size_kb = bak.stat().st_size / 1024
            if size_kb > 50:
                bloated.append(f"{bak.name} ({size_kb:.0f}KB)")

        assert not bloated, (
            f"Backup files bloated (contain residual MCP entries): {bloated}."
            f" The gateway's size-drop guard will restore stale data from these on restart,"
            f" causing cleanup to be ineffective."
            f" Fix: rm ~/.openclaw/openclaw.json.bak* ~/.openclaw/openclaw.json.last-good"
            f" && python scripts/check_openclaw_env.py --fix"
        )

    def test_openclaw_config_values_correct(self):
        """Verify key runtime parameters set by configure_openclaw.py."""
        if not OPENCLAW_CONFIG.exists():
            pytest.skip("openclaw config does not exist")
        with open(OPENCLAW_CONFIG) as f:
            config = json.load(f)

        defaults = config.get("agents", {}).get("defaults", {})
        problems: list[str] = []

        # contextWindow — all models must have contextWindow >= 128000
        providers = config.get("models", {}).get("providers", {})
        for pname, provider in providers.items():
            for model in provider.get("models", []):
                cw = model.get("contextWindow", 0)
                if cw < 128000:
                    problems.append(
                        f"models.providers.{pname}.{model.get('id')}"
                        f".contextWindow={cw} (requires >= 128000)"
                    )

        # reserveTokensFloor — should not equal contextWindow (disables compaction)
        rtf = defaults.get("compaction", {}).get("reserveTokensFloor", 0)
        if rtf >= 200000:
            problems.append(
                f"reserveTokensFloor={rtf} (too large, disables compaction causing context bloat)"
            )

        # temperature = 0
        temp = defaults.get("params", {}).get("temperature")
        if temp is None or temp != 0:
            problems.append(f"agents.defaults.params.temperature={temp} (should be 0)")

        # heartbeat disabled
        hb = defaults.get("heartbeat", {}).get("every", "")
        if hb != "0m":
            problems.append(f"agents.defaults.heartbeat.every='{hb}' (should be '0m')")

        # skipBootstrap
        if not defaults.get("skipBootstrap"):
            problems.append("agents.defaults.skipBootstrap not set to true")

        assert not problems, (
            f"openclaw config values incorrect: {problems}."
            f" Fix: python scripts/configure_openclaw.py"
        )

    def test_tasks_directory_populated(self):
        tasks_dir = CLAW_EVAL_DIR / "tasks"
        assert tasks_dir.exists(), f"Tasks directory does not exist: {tasks_dir}"
        assert list(tasks_dir.glob("*/task.yaml")), (
            f"No task.yaml found under {tasks_dir} — submodule may not be fully pulled"
        )

    def test_model_config_complete(self):
        """model / judge triplet (api_key/base_url/model_id) must be complete."""
        cfg_path = CLAW_EVAL_DIR / "config.yaml"
        assert cfg_path.exists(), f"{cfg_path} does not exist"
        with open(cfg_path) as f:
            data = yaml.safe_load(f) or {}

        for role, env_prefix in [("model", "MODEL"), ("judge", "JUDGE")]:
            section = data.get(role, {})
            api_key = section.get("api_key") or os.environ.get(f"{env_prefix}_API_KEY", "")
            base_url = section.get("base_url") or os.environ.get(f"{env_prefix}_BASE_URL", "")
            model_id = section.get("model_id") or os.environ.get(f"{env_prefix}_MODEL_ID", "")
            missing = [k for k, v in {
                "api_key": api_key, "base_url": base_url, "model_id": model_id
            }.items() if not v]
            assert not missing, (
                f"{role} config missing: {missing}."
                f" Fix: python scripts/configure_model.py --api-key sk-XXX"
            )


# ────────────────────────────────────────────────────────────────────────────
# Runtime — must be ready before running tasks
# ────────────────────────────────────────────────────────────────────────────

class TestRuntime:
    """Runtime state that must be satisfied before starting tasks."""

    def test_gateway_running_and_healthy(self):
        """openclaw gateway must be running and /health returns 200."""
        if not OPENCLAW_CONFIG.exists():
            pytest.skip("openclaw config does not exist")
        with open(OPENCLAW_CONFIG) as f:
            config = json.load(f)
        port = config.get("gateway", {}).get("port")
        if not port:
            pytest.skip("gateway.port not configured")

        try:
            import httpx
            r = httpx.get(f"http://127.0.0.1:{port}/health", timeout=3)
        except Exception as e:
            pytest.fail(
                f"gateway (127.0.0.1:{port}) unreachable: {e}."
                f" Fix: openclaw gateway start"
            )
        assert r.status_code == 200, (
            f"gateway /health returned {r.status_code}."
            f" Fix: openclaw gateway restart"
        )

    def test_mock_ports_free(self):
        """Mock services ports 9100-9116 must be free (no residual processes)."""
        occupied = [p for p in MOCK_PORTS if not _port_free(p)]
        assert not occupied, (
            f"Mock services ports occupied: {occupied}."
            f" Usually residual mock_services processes from previous runs."
            f" Fix: python scripts/check_openclaw_env.py --fix"
            f" or pkill -f mock_services"
        )

    def test_no_residual_claweval_artifacts(self):
        """Detect residual claweval-* agents / mcp.servers / directories from previous runs."""
        if not OPENCLAW_CONFIG.exists():
            pytest.skip("openclaw config does not exist")
        with open(OPENCLAW_CONFIG) as f:
            config = json.load(f)

        problems: list[str] = []
        agents = config.get("agents", {}).get("list", [])
        leftover_agents = [a["id"] for a in agents
                           if a.get("id", "").startswith("claweval-")]
        if leftover_agents:
            problems.append(f"Residual agents: {leftover_agents}")

        servers = config.get("mcp", {}).get("servers", {})
        leftover_servers = [k for k in servers
                           if k.startswith(("claw-eval-", "ce-mock-", "ce-sb-"))]
        if leftover_servers:
            problems.append(f"Residual mcp.servers: {len(leftover_servers)}")

        leftover_dirs = list(
            (Path.home() / ".openclaw").glob("workspace-claweval-*")
        )
        if leftover_dirs:
            problems.append(f"Residual workspace directories: {len(leftover_dirs)}")

        assert not problems, (
            f"Detected residual artifacts from previous run: {problems}."
            f" Fix: python scripts/check_openclaw_env.py --fix"
        )


# ────────────────────────────────────────────────────────────────────────────
# Sandbox — only needed for --sandbox mode
# ────────────────────────────────────────────────────────────────────────────

class TestSandbox:
    """Sandbox mode prerequisites (M-series tasks, C tasks with sandbox_files)."""

    def test_docker_cli(self):
        if not shutil.which("docker"):
            pytest.skip("docker not installed — only affects --sandbox mode")

    def test_docker_daemon_running(self):
        if not shutil.which("docker"):
            pytest.skip("docker not installed")
        result = subprocess.run(
            ["docker", "info"], capture_output=True, text=True, timeout=10
        )
        assert result.returncode == 0, (
            "docker daemon not running or current user lacks permission."
            " Fix: systemctl start docker; usermod -aG docker $USER && newgrp docker"
        )

    def test_sandbox_image_built(self):
        if not shutil.which("docker"):
            pytest.skip("docker not installed")
        result = subprocess.run(
            ["docker", "image", "inspect", SANDBOX_IMAGE],
            capture_output=True, text=True, timeout=10,
        )
        assert result.returncode == 0, (
            f"Sandbox image {SANDBOX_IMAGE} does not exist."
            f" Fix: docker build -f claw-eval/Dockerfile.agent"
            f" -t {SANDBOX_IMAGE} claw-eval/"
        )

    def test_no_stale_sandbox_containers(self):
        """Detect residual claw-eval containers from previous runs occupying sandbox ports."""
        if not shutil.which("docker"):
            pytest.skip("docker not installed")
        result = subprocess.run(
            ["docker", "container", "ls", "-a", "-q",
             "--filter", "label=app=claw-eval"],
            capture_output=True, text=True, timeout=10,
        )
        if result.returncode != 0:
            pytest.skip("docker container ls failed")
        containers = result.stdout.strip().splitlines()
        assert not containers, (
            f"Detected {len(containers)} residual claw-eval container(s),"
            f" occupying sandbox ports (20000+) which will cause all batch runs to fail."
            f" Fix: python scripts/check_openclaw_env.py --fix"
            f" or docker rm -f $(docker container ls -aq --filter label=app=claw-eval)"
        )


# ────────────────────────────────────────────────────────────────────────────
# Optional — missing only affects some features
# ────────────────────────────────────────────────────────────────────────────

class TestOptional:
    """Optional components — missing only results in skip, not failure."""

    def test_node_npm(self):
        if not shutil.which("npm"):
            pytest.skip("npm not installed — mcporter unavailable, some MCP tools limited")

    def test_mcporter_installed(self):
        if not shutil.which("npm"):
            pytest.skip("npm not installed")
        result = subprocess.run(
            ["npm", "list", "-g", "mcporter", "--depth=0"],
            capture_output=True, text=True, timeout=15,
        )
        if "mcporter" not in result.stdout:
            pytest.skip("mcporter not installed globally. Fix: npm install -g mcporter")
