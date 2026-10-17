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

"""Tests for scripts/check_openclaw_env.py."""
from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
from unittest.mock import patch

import pytest


SCRIPT_PATH = Path(__file__).resolve().parents[1] / "scripts" / "check_openclaw_env.py"


@pytest.fixture(scope="module")
def script_mod():
    spec = importlib.util.spec_from_file_location("test_check_openclaw_env", SCRIPT_PATH)
    mod = importlib.util.module_from_spec(spec)
    sys.modules["test_check_openclaw_env"] = mod
    spec.loader.exec_module(mod)  # type: ignore[union-attr]
    return mod


def test_missing_config_is_not_an_issue(script_mod, tmp_path):
    """A machine that never ran openclaw is clean, not artifact-polluted."""
    missing = tmp_path / "openclaw.json"
    with patch.object(script_mod, "OPENCLAW_CONFIG", missing):
        assert script_mod.check_config() == []


def test_config_with_claweval_agent_is_an_issue(script_mod, tmp_path):
    cfg = tmp_path / "openclaw.json"
    cfg.write_text(
        '{"agents": {"list": [{"id": "claweval-t1"}]},'
        ' "mcp": {"servers": {}}, "tools": {}}'
    )
    with patch.object(script_mod, "OPENCLAW_CONFIG", cfg):
        issues = script_mod.check_config()
    assert any("claweval agents in config" in i for i in issues)


def test_fix_env_succeeds_when_config_absent(script_mod, tmp_path, capsys):
    """--fix must not report failure merely because openclaw.json is absent."""
    missing = tmp_path / "openclaw.json"
    with patch.object(script_mod, "OPENCLAW_CONFIG", missing), \
         patch.object(script_mod, "_fix_filesystem",
                      return_value=["removed workspace dir"]), \
         patch.object(script_mod, "check_filesystem", return_value=[]), \
         patch.object(script_mod, "check_processes", return_value=[]), \
         patch.object(script_mod, "check_docker_containers", return_value=[]):
        assert script_mod.fix_env() is True
    out = capsys.readouterr().out
    assert "issue(s) remain" not in out