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

"""Tests for OpenClaw tokenless evidence collection."""

from __future__ import annotations

import json
from pathlib import Path

from swe_runner.agents.openclaw.tokenless_evidence import write_tokenless_evidence


def test_write_tokenless_evidence_records_weak_evidence_when_files_are_missing(tmp_path: Path) -> None:
    metadata = {
        "agent_id": "case-1",
        "session_id": "session-1",
        "openclaw_profile_dir": str(tmp_path / "missing-profile"),
        "openclaw_config_path": str(tmp_path / "missing-config.json"),
        "openclaw_workspace_root": str(tmp_path / "missing-workspace"),
    }

    updates = write_tokenless_evidence(
        output_dir=tmp_path / "run",
        instance_id="django/django#13448",
        metadata=metadata,
        raw_output="ordinary OpenClaw output",
    )

    assert updates["openclaw_tokenless_evidence_strong"] == "false"
    assert updates["openclaw_tokenless_plugin_loaded"] == "false"
    assert updates["openclaw_tokenless_hook_seen"] == "false"
    assert updates["openclaw_tokenless_exec_tool_calls"] == "0"

    evidence_path = Path(updates["openclaw_tokenless_evidence_path"])
    assert evidence_path.name == "django-django-13448.json"
    evidence = json.loads(evidence_path.read_text(encoding="utf-8"))
    assert evidence["strong"] is False
    assert evidence["reasons"] == {
        "config_enabled": False,
        "sandbox_binaries_present": False,
        "profile_extension_present": False,
        "plugin_loaded": False,
        "hook_seen": False,
        "exec_tool_calls": 0,
    }
