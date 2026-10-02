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

"""Tests for per-instance input manifests."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
from unittest.mock import patch

from swe_runner.agents.lifecycle import PreparedAgentRun
from swe_runner.common.models import AgentConfig, Settings, SWEInstance
from swe_runner.run.io.input_manifest import write_input_manifest


def _sha256_text(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def _make_instance() -> SWEInstance:
    return SWEInstance(
        instance_id="django__django-1",
        repo="django/django",
        version="3.2",
        base_commit="abc123",
        problem_statement="Fix the bug",
        patch="gold patch",
        test_patch="test patch",
    )


def test_write_input_manifest_records_hashes_and_redacts_openclaw_config(tmp_path: Path) -> None:
    agents_path = tmp_path / "openclaw-workspace" / "AGENTS.md"
    agents_path.parent.mkdir(parents=True)
    agents_path.write_text("# Repository Task Guidelines\n", encoding="utf-8")

    profile_dir = tmp_path / "run" / "openclaw-profiles" / "django__django-1"
    profile_dir.mkdir(parents=True)
    config_path = profile_dir / "openclaw.json"
    config_path.write_text(
        json.dumps(
            {
                "providers": {
                    "bailian-token-plan": {"apiKey": "secret-value", "baseURL": "https://example.test"},
                    "openai": {"apiKey": "secret-value", "baseURL": "https://openai.example.test"},
                },
                "agents": {"defaults": {"skipBootstrap": True}, "params": {"temperature": 0, "top_p": 1}},
            }
        ),
        encoding="utf-8",
    )

    prepared = PreparedAgentRun(
        instance=_make_instance(),
        settings=Settings(
            agent=AgentConfig(name="openclaw", timeout=1200, step_limit=0, workers=2),
            output={"output_dir": tmp_path / "run"},
        ),
        work_dir=tmp_path / "repo",
        prompt="Solve the following issue.",
        timeout=1200,
        max_turns=0,
        base_revision="abc123",
        metadata={
            "docker_image_name": "swebench/sweb.eval.x86_64.django_1776_django-1:latest",
            "openclaw_agents_path": str(agents_path),
            "openclaw_config_path": str(config_path),
            "openclaw_profile": "swebench-django__django-1-deadbeef",
            "openclaw_profile_dir": str(profile_dir),
            "openclaw_injection_mode": "common",
        },
    )

    with patch(
        "swe_runner.run.io.input_manifest._runner_git_info",
        return_value={
            "repo_root": "/repo",
            "commit": "rev",
            "dirty": False,
            "status_porcelain_sha256": _sha256_text(""),
        },
    ):
        manifest_path = write_input_manifest(tmp_path / "run", agent_name="openclaw", prepared=prepared)

    assert manifest_path == tmp_path / "run" / "input-manifests" / "django__django-1" / "input_manifest.json"
    data = json.loads(manifest_path.read_text(encoding="utf-8"))

    assert data["schema_version"] == 1
    assert data["instance_id"] == "django__django-1"
    assert data["agent_name"] == "openclaw"
    assert data["dataset"]["repo"] == "django/django"
    assert data["dataset"]["problem_statement"]["sha256"] == _sha256_text("Fix the bug")
    assert data["dataset"]["reference_patch"]["sha256"] == _sha256_text("gold patch")
    assert data["prompt"]["sha256"] == _sha256_text("Solve the following issue.")
    assert data["runtime"]["docker_image_name"] == "swebench/sweb.eval.x86_64.django_1776_django-1:latest"
    assert data["files"]["openclaw_agents"]["sha256"] == _sha256_text("# Repository Task Guidelines\n")
    assert data["files"]["openclaw_config"]["exists"] is True
    assert data["files"]["openclaw_profile_dir"]["file_count"] == 1
    assert data["openclaw"]["config_redacted"]["providers"]["bailian-token-plan"]["apiKey"] == "<redacted>"
    assert data["openclaw"]["config_redacted"]["providers"]["bailian-token-plan"]["baseURL"] == "https://example.test"
    assert data["openclaw"]["config_redacted"]["providers"]["openai"]["apiKey"] == "<redacted>"
    assert data["openclaw"]["config_redacted"]["providers"]["openai"]["baseURL"] == "https://openai.example.test"
    assert data["openclaw"]["config_redacted"]["agents"]["params"]["temperature"] == 0


def test_write_input_manifest_records_configured_builtin_skill_when_enabled(tmp_path: Path) -> None:
    skills_dir = tmp_path / "skills"
    skill_path = skills_dir / "swe-bench-patch-generation" / "SKILL.md"
    skill_path.parent.mkdir(parents=True)
    skill_path.write_text("# SWE-bench Patch Generation\n", encoding="utf-8")

    prepared = PreparedAgentRun(
        instance=_make_instance(),
        settings=Settings(
            agent=AgentConfig(
                name="openclaw",
                timeout=1200,
                step_limit=0,
                workers=2,
                use_skill=True,
                skills_dir=skills_dir,
            ),
            output={"output_dir": tmp_path / "run"},
        ),
        work_dir=tmp_path / "repo",
        prompt="Solve the following issue.",
        timeout=1200,
        max_turns=0,
        base_revision="abc123",
        metadata={
            "docker_image_name": "swebench/sweb.eval.x86_64.django_1776_django-1:latest",
            "openclaw_injection_mode": "skill",
        },
    )

    with patch(
        "swe_runner.run.io.input_manifest._runner_git_info",
        return_value={
            "repo_root": "/repo",
            "commit": "rev",
            "dirty": False,
            "status_porcelain_sha256": _sha256_text(""),
        },
    ):
        manifest_path = write_input_manifest(tmp_path / "run", agent_name="openclaw", prepared=prepared)

    data = json.loads(manifest_path.read_text(encoding="utf-8"))
    builtin_skill = data["files"]["builtin_skill"]

    assert builtin_skill["exists"] is True
    assert builtin_skill["path"] == str(skill_path)
    assert builtin_skill["bytes"] > 0
    assert len(builtin_skill["sha256"]) == 64


def test_write_input_manifest_omits_openclaw_section_for_other_agents(tmp_path: Path) -> None:
    prepared = PreparedAgentRun(
        instance=_make_instance(),
        settings=Settings(
            agent=AgentConfig(name="cosh", timeout=1200, step_limit=0, workers=2),
            output={"output_dir": tmp_path / "run"},
        ),
        work_dir=tmp_path / "repo",
        prompt="Solve the following issue.",
        timeout=1200,
        max_turns=0,
        base_revision="abc123",
        metadata={
            "docker_image_name": "swebench/sweb.eval.x86_64.django_1776_django-1:latest",
        },
    )

    with patch(
        "swe_runner.run.io.input_manifest._runner_git_info",
        return_value={
            "repo_root": "/repo",
            "commit": "rev",
            "dirty": False,
            "status_porcelain_sha256": _sha256_text(""),
        },
    ):
        manifest_path = write_input_manifest(tmp_path / "run", agent_name="cosh", prepared=prepared)

    data = json.loads(manifest_path.read_text(encoding="utf-8"))

    assert "openclaw" not in data
    assert "openclaw_agents" not in data["files"]
    assert "openclaw_config" not in data["files"]
    assert "openclaw_profile_dir" not in data["files"]
