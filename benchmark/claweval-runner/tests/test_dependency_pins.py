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

"""Dependency contract tests.

The runtime modules import ``mcp.server.fastmcp``, an API the MCP SDK removed
in 2.x (FastMCP was renamed to ``MCPServer`` and the module now raises
``ModuleNotFoundError`` on import). The declared requirement must therefore
exclude 2.x so a fresh ``pip install`` cannot resolve to an SDK that breaks
``mcp_mock_services`` and ``mcp_sandbox_tools`` at import time.
"""

import re
from pathlib import Path

import pytest

PYPROJECT = Path(__file__).resolve().parent.parent / "pyproject.toml"


def _requirement(name: str) -> str:
    match = re.search(
        rf'^\s*"{re.escape(name)}([^"]*)"\s*,?\s*$',
        PYPROJECT.read_text(encoding="utf-8"),
        flags=re.MULTILINE,
    )
    assert match, f"{name} must be declared in pyproject.toml dependencies"
    return name + match.group(1)


def _upper_bound(requirement: str) -> float | None:
    for comparator, version in re.findall(r"([<>]=?|==)\s*([0-9]+(?:\.[0-9]+)*)", requirement):
        if comparator in ("<", "<=", "=="):
            return float(version)
    return None


def test_mcp_requirement_excludes_sdk_v2() -> None:
    requirement = _requirement("mcp")
    bound = _upper_bound(requirement)
    assert bound is not None and bound <= 2.0, (
        "mcp.server.fastmcp was removed in mcp 2.x; the declared requirement "
        f"must keep an upper bound below 2, got {requirement!r}"
    )


def test_mcp_requirement_keeps_v1_floor() -> None:
    requirement = _requirement("mcp")
    floors = [
        float(version)
        for comparator, version in re.findall(r"([<>]=?|==)\s*([0-9]+(?:\.[0-9]+)*)", requirement)
        if comparator in (">", ">=")
    ]
    assert floors and floors[0] >= 1.0, (
        f"the mcp requirement must keep the v1 floor, got {requirement!r}"
    )


if __name__ == "__main__":  # pragma: no cover
    pytest.main([__file__])
