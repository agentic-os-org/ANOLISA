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

"""Run settings give every consumer the same configured home paths."""

from pathlib import Path
from unittest.mock import patch

import pytest
from typer.testing import CliRunner

from swe_runner.agents.openclaw.adapter import _resolve_openclaw_agents_text
from swe_runner.agents.openclaw.profile import OpenClawCaseProfileManager
from swe_runner.cli import app
from swe_runner.common.models import AgentConfig, OutputConfig, Settings
from swe_runner.run.io.output_store import RunOutputStore
from swe_runner.run.io.report import RunReport
from swe_runner.run.prompting.prompt_resources import SKILL_NAME, load_custom_prompt


@pytest.fixture
def private_home(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> Path:
    home = tmp_path / "home"
    home.mkdir()
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.chdir(tmp_path)
    return home


@pytest.mark.parametrize("field", ["skills_dir", "prompts_dir"])
def test_agent_settings_expand_home_before_resource_resolution(private_home: Path, field: str) -> None:
    config = AgentConfig(name="openclaw", **{field: "~/guidance"})
    assert getattr(config, field) == private_home / "guidance"


def test_run_output_and_openclaw_profile_share_one_root(private_home: Path, tmp_path: Path) -> None:
    settings = Settings(output=OutputConfig(output_dir="~/outputs"))
    store = RunOutputStore(settings.output.output_dir)
    links = tmp_path / "links"
    links.mkdir()
    profiles = OpenClawCaseProfileManager(output_dir=settings.output.output_dir, profile_link_root=links)
    profile = profiles.prepare("private-case")
    try:
        assert store.output_dir.resolve() == private_home / "outputs"
        assert profile.directory.parent.parent == store.output_dir.resolve()
        assert not (tmp_path / "~").exists()
    finally:
        profiles.cleanup_link(profile)


def test_custom_prompt_and_skill_load_from_expanded_settings(private_home: Path) -> None:
    (private_home / "prompts").mkdir()
    (private_home / "prompts/private-case").write_text("private custom guidance", encoding="utf-8")
    skill = private_home / "skills" / SKILL_NAME / "SKILL.md"
    skill.parent.mkdir(parents=True)
    skill.write_text("private skill guidance", encoding="utf-8")
    custom = AgentConfig(name="cosh", prompts_dir="~/prompts", per_case_prompt=True)
    assert load_custom_prompt("private-case", prompts_dir=custom.prompts_dir) == "private custom guidance"
    config = AgentConfig(name="openclaw", skills_dir="~/skills", use_skill=True)
    mode, text = _resolve_openclaw_agents_text(Settings(agent=config))
    assert mode == "skill"
    assert text and "private skill guidance" in text


def test_actual_run_cli_expands_paths_before_logging_and_session(private_home: Path) -> None:
    report = RunReport(succeeded=0, failed=0, total=0, instance_ids=[])
    with (
        patch("swe_runner.cli_commands.setup_logging") as logging,
        patch("swe_runner.run.session.RunSession.execute", autospec=True, return_value=report) as execute,
    ):
        result = CliRunner().invoke(
            app, ["run", "--agent", "cosh", "--output", "~/outputs", "--per-case-prompt", "--prompts-dir", "~/prompts"]
        )
    assert result.exit_code == 0, result.output
    assert logging.call_args.args[0] == private_home / "outputs/run"
    settings = execute.call_args.args[0]._settings
    assert settings.output.output_dir == private_home / "outputs/run"
    assert settings.agent.prompts_dir == private_home / "prompts"


@pytest.mark.parametrize("path", ["relative/path", "literal~name", "/tmp/absolute"])
def test_other_paths_and_optional_defaults_are_preserved(path: str) -> None:
    assert OutputConfig(output_dir=path).output_dir == Path(path)
    config = AgentConfig(name="cosh", skills_dir=path, prompts_dir=path)
    assert config.skills_dir == Path(path)
    assert config.prompts_dir == Path(path)
    assert AgentConfig(name="cosh").skills_dir is None
    assert AgentConfig(name="cosh").prompts_dir is None
