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

"""Legacy skill generator — now a no-op.

Previously generated TOOLS.md + mcporter configs to let agents call MCP tools
via ``exec`` + ``mcporter call``.  Since the always-sandbox refactor, agents
use the gateway's native MCP bridge (tools.allow with serverKey__toolName),
so TOOLS.md / mcporter are no longer needed.

The public API is kept as stubs so existing callers don't break.
"""

from __future__ import annotations

from pathlib import Path

from ._common import task_agent_id

_OPENCLAW_DIR = Path.home() / ".openclaw"
_MCPORTER_DIR = _OPENCLAW_DIR / "mcporter"


def generate_task_skill(task_yaml: str, task_id: str, port_offset: int = 0) -> None:
    """No-op: MCP tools are now exposed via gateway bridge, not mcporter."""
    pass


def cleanup_task_skill(task_id: str) -> None:
    """Remove any leftover TOOLS.md and mcporter config from previous runs."""
    agent_id = task_agent_id(task_id)

    mcporter = _MCPORTER_DIR / f"claw-eval-{task_id.lower()}.json"
    if mcporter.exists():
        mcporter.unlink(missing_ok=True)

    workspace = _OPENCLAW_DIR / f"workspace-{agent_id.lower()}"
    tools_md = workspace / "TOOLS.md"
    if tools_md.exists():
        tools_md.unlink(missing_ok=True)
