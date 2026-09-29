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

"""Pytest fixtures for ce-runner test cleanup.

Provides:
- ``CleanupContext``: snapshot/restore environment state
- ``cleanup_session``: session-level guard (snapshot at start, restore at end)
"""

import json
from dataclasses import dataclass, field
from pathlib import Path

import pytest

OPENCLAW_DIR = Path.home() / ".openclaw"
OPENCLAW_CONFIG = OPENCLAW_DIR / "openclaw.json"
GATEWAY_PORT = 18789


def _import_cleanup():
    """Lazy-import production cleanup functions to avoid collection-time errors."""
    import sys
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))
    from ce_runner.infra import cleanup_config, cleanup_mock_services, restart_gateway
    return cleanup_config, cleanup_mock_services, restart_gateway


@dataclass
class CleanupContext:
    """Captures environment baseline and restores to it.

    Delegates to existing production functions (``cleanup_config``,
    ``cleanup_mock_services``) rather than reimplementing logic.

    Usage outside fixtures::

        ctx = CleanupContext()
        ctx.snapshot()
        # ... run test ...
        ctx.restore()
        issues = ctx.verify_clean()
    """

    baseline_agent_ids: set[str] = field(default_factory=set)
    baseline_mcp_keys: set[str] = field(default_factory=set)

    # -- snapshot -----------------------------------------------------------

    def snapshot(self):
        """Record current environment state as baseline."""
        self.baseline_agent_ids = self._read_claweval_agents()
        self.baseline_mcp_keys = self._read_claweval_mcp_keys()

    # -- restore ------------------------------------------------------------

    def restore(self):
        """Remove artifacts introduced since snapshot.

        Uses ``cleanup_config(context=None)`` for a full sweep of all
        claweval-* agents, MCP servers, and directories.  Also kills
        leftover mock_services processes.

        Idempotent -- safe to call multiple times.
        """
        cleanup_config, cleanup_mock_services, restart_gateway = _import_cleanup()
        try:
            cleanup_mock_services()
        except Exception:
            pass
        try:
            cleanup_config(context=None, skip_dirs=False)
        except Exception:
            pass
        try:
            restart_gateway(str(OPENCLAW_CONFIG), GATEWAY_PORT)
        except Exception:
            pass

    # -- verify -------------------------------------------------------------

    def verify_clean(self) -> list[str]:
        """Return list of leaked artifacts (empty list = environment is clean)."""
        issues: list[str] = []
        agents = self._read_claweval_agents()
        if agents:
            issues.append(f"Leaked agents in config: {sorted(agents)}")
        mcp = self._read_claweval_mcp_keys()
        if mcp:
            issues.append(f"Leaked MCP server keys: {sorted(mcp)}")
        if OPENCLAW_DIR.exists():
            ws = list(OPENCLAW_DIR.glob("workspace-claweval-*"))
            if ws:
                issues.append(f"Leaked workspace dirs ({len(ws)})")
            agents_dir = OPENCLAW_DIR / "agents"
            if agents_dir.exists():
                ad = list(agents_dir.glob("claweval-*"))
                if ad:
                    issues.append(f"Leaked agent dirs ({len(ad)})")
        return issues

    # -- internal helpers ---------------------------------------------------

    @staticmethod
    def _read_claweval_agents() -> set[str]:
        if not OPENCLAW_CONFIG.exists():
            return set()
        with open(OPENCLAW_CONFIG) as f:
            config = json.load(f)
        return {
            a["id"] for a in config.get("agents", {}).get("list", [])
            if a.get("id", "").lower().startswith("claweval-")
        }

    @staticmethod
    def _read_claweval_mcp_keys() -> set[str]:
        if not OPENCLAW_CONFIG.exists():
            return set()
        with open(OPENCLAW_CONFIG) as f:
            config = json.load(f)
        return {
            k for k in config.get("mcp", {}).get("servers", {})
            if k.startswith(("claw-eval-", "ce-mock-", "ce-sb-"))
        }


# ── Fixtures ──────────────────────────────────────────────────────────────

@pytest.fixture(scope="session", autouse=True)
def cleanup_session():
    """Session-level environment guard.

    Snapshots clean state before any tests run, then at session end
    cleans up all claweval-* artifacts and warns of any remaining leaks.
    """
    ctx = CleanupContext()
    ctx.snapshot()

    yield ctx

    ctx.restore()
    issues = ctx.verify_clean()
    if issues:
        import sys
        print(
            f"\n[conftest] WARNING: {len(issues)} environmental leak(s) "
            f"detected after session cleanup:",
            file=sys.stderr,
        )
        for issue in issues:
            print(f"  - {issue}", file=sys.stderr)
