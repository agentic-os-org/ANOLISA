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

from __future__ import annotations

from pathlib import Path

import pytest

from swe_runner.cli_commands import CommandUsageError, build_run_settings


def test_build_run_settings_maps_cli_values_to_settings(tmp_path: Path) -> None:
    settings = build_run_settings(
        agent="openclaw",
        subset="lite",
        split="test",
        output=tmp_path,
        timeout=120,
        step_limit=10,
        slice_range="0:2",
        filter_regex="django",
        instance_id="a, b",
        workers=2,
        docker_pull_registry="mirror.local",
        use_skill=False,
        skills_dir=tmp_path / "skills",
        tokenless=True,
        per_case_prompt=True,
        prompts_dir=tmp_path / "prompts",
    )

    assert settings.agent.name == "openclaw"
    assert settings.agent.timeout == 120
    assert settings.agent.step_limit == 10
    assert settings.agent.workers == 2
    assert settings.agent.docker_pull_registry == "mirror.local"
    assert settings.agent.skills_dir == tmp_path / "skills"
    assert settings.agent.tokenless is True
    assert settings.agent.per_case_prompt is True
    assert settings.agent.prompts_dir == tmp_path / "prompts"
    assert settings.dataset.subset == "lite"
    assert settings.dataset.split == "test"
    assert settings.dataset.slice_range == "0:2"
    assert settings.dataset.filter_regex == "django"
    assert settings.dataset.instance_ids == ["a", "b"]
    assert settings.output.output_dir == tmp_path / "run"


def test_build_run_settings_rejects_conflicting_prompt_guidance(tmp_path: Path) -> None:
    with pytest.raises(CommandUsageError, match="mutually exclusive"):
        build_run_settings(
            agent="openclaw",
            subset="lite",
            split="test",
            output=tmp_path,
            timeout=120,
            step_limit=0,
            slice_range=None,
            filter_regex=None,
            instance_id=None,
            workers=1,
            docker_pull_registry=None,
            use_skill=True,
            tokenless=False,
            per_case_prompt=True,
        )


def test_build_run_settings_rejects_unsupported_tokenless_agent(tmp_path: Path) -> None:
    with pytest.raises(CommandUsageError, match="not supported"):
        build_run_settings(
            agent="cosh",
            subset="lite",
            split="test",
            output=tmp_path,
            timeout=120,
            step_limit=0,
            slice_range=None,
            filter_regex=None,
            instance_id=None,
            workers=1,
            docker_pull_registry=None,
            use_skill=False,
            tokenless=True,
            per_case_prompt=False,
        )
