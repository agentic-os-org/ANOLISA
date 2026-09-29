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

"""Unit tests for the OpenClaw local-profile adapter."""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import MagicMock, patch

from swe_runner.agents import get_agent
from swe_runner.agents.openclaw.adapter import (
    OpenClawAdapter,
    build_openclaw_agent_id,
    build_openclaw_session_id,
)
from swe_runner.agents.openclaw.client import OpenClawClient
from swe_runner.agents.openclaw.profile import OpenClawCaseProfileManager
from swe_runner.agents.openclaw.sandbox import build_openclaw_agent_scope_key
from swe_runner.common.commands import CommandResult
from swe_runner.common.models import AgentConfig, Settings, SWEInstance


def make_instance(instance_id: str = "django__django-13448") -> SWEInstance:
    return SWEInstance(
        instance_id=instance_id,
        repo="django/django",
        version="1",
        base_commit="abc123",
        problem_statement="Fix it",
        patch="",
        test_patch="",
    )


def make_settings(
    tmp_path: Path,
    *,
    tokenless: bool = False,
    use_skill: bool = False,
    per_case_prompt: bool = False,
) -> Settings:
    return Settings(
        agent=AgentConfig(
            name="openclaw",
            timeout=1800,
            step_limit=200,
            workers=2,
            tokenless=tokenless,
            use_skill=use_skill,
            per_case_prompt=per_case_prompt,
        ),
        output={"output_dir": tmp_path / "run"},
    )


def test_profile_manager_copies_base_config_and_creates_symlink(tmp_path: Path) -> None:
    base_config = tmp_path / "base-openclaw.json"
    base_config.write_text('{"agents": {"list": [{"id": "main"}]}}', encoding="utf-8")
    link_root = tmp_path / "home"
    link_root.mkdir()
    manager = OpenClawCaseProfileManager(
        output_dir=tmp_path / "run",
        base_config_path=base_config,
        profile_link_root=link_root,
    )

    profile = manager.prepare("django__django-13448")

    assert profile.config_path.read_text(encoding="utf-8") == base_config.read_text(encoding="utf-8")
    assert profile.link_path.is_symlink()
    assert profile.link_path.resolve() == profile.directory.resolve()

    manager.cleanup_link(profile)

    assert not profile.link_path.exists()
    assert profile.directory.exists()


def test_prepare_creates_case_profile_and_single_local_sandbox_agent(tmp_path: Path) -> None:
    base_config = tmp_path / "base-openclaw.json"
    base_config.write_text('{"agents": {"list": [{"id": "main"}]}}', encoding="utf-8")
    workspace_root = tmp_path / "workspace"
    openclaw_workspace_root = workspace_root / "openclaw-workspace"
    work_dir = workspace_root / "repo"
    link_root = tmp_path / "home"
    link_root.mkdir()

    commands: list[list[str]] = []

    def fake_prepare_workspace(*args: object, **kwargs: object) -> Path:
        work_dir.mkdir(parents=True)
        return work_dir

    def fake_run(cmd: list[str], **kwargs: object) -> CommandResult:
        commands.append(cmd)
        if cmd[:2] == ["docker", "ps"]:
            return CommandResult(args=tuple(cmd), returncode=0, stdout="stale-container\n", stderr="")
        if cmd[:3] == ["docker", "rm", "-f"]:
            return CommandResult(args=tuple(cmd), returncode=0, stdout="", stderr="")

        assert cmd[:3] == ["openclaw", "--profile", cmd[2]]
        return CommandResult(
            args=tuple(cmd),
            returncode=0,
            stdout=json.dumps({"sandbox": {"workspaceRoot": str(openclaw_workspace_root)}}),
            stderr="",
        )

    agent = OpenClawAdapter(base_config_path=base_config, profile_link_root=link_root)
    with (
        patch("swe_runner.agents.openclaw.adapter.default_workspace_root", return_value=workspace_root),
        patch("swe_runner.agents.openclaw.adapter.prepare_workspace_from_image", side_effect=fake_prepare_workspace),
        patch("swe_runner.agents.openclaw.adapter.get_git_revision", return_value="base-rev"),
        patch("swe_runner.agents.openclaw.adapter.build_openclaw_prompt", return_value="fix the bug"),
        patch("swe_runner.agents.openclaw.sandbox.run_command", side_effect=fake_run),
    ):
        prepared = agent.prepare(make_instance(), make_settings(tmp_path))

    config_path = Path(prepared.metadata["openclaw_config_path"])
    config = json.loads(config_path.read_text(encoding="utf-8"))
    agent_entries = config["agents"]["list"]
    local_entry = next(item for item in agent_entries if item["id"] == "django__django-13448")

    assert prepared.prompt == "fix the bug"
    assert prepared.metadata["agent_id"] == local_entry["id"]
    assert prepared.metadata["base_agent_id"] == "swebench"
    assert prepared.metadata["session_id"].startswith("django__django-13448-")
    assert not prepared.metadata["session_id"].startswith("django__django-13448-django__django-13448-")
    assert prepared.metadata["openclaw_profile"].startswith("swebench-django__django-13448-")
    assert Path(prepared.metadata["openclaw_profile_dir"]).parent == tmp_path / "run" / "openclaw-profiles"
    agents_path = openclaw_workspace_root / "AGENTS.md"
    assert prepared.metadata["openclaw_injection_mode"] == "common"
    assert prepared.metadata["openclaw_agents_path"] == str(agents_path)
    assert "Repository Task Guidelines" in agents_path.read_text(encoding="utf-8")
    assert config["agents"]["defaults"]["skipBootstrap"] is True
    assert local_entry["workspace"] == str(openclaw_workspace_root)
    assert local_entry["sandbox"]["workspaceRoot"] == str(openclaw_workspace_root)
    assert local_entry["sandbox"]["workspaceAccess"] == "rw"
    assert local_entry["sandbox"]["scope"] == "agent"
    assert local_entry["sandbox"]["docker"]["image"] == "swebench/sweb.eval.x86_64.django_1776_django-13448:latest"
    assert "user" not in local_entry["sandbox"]["docker"]
    assert local_entry["sandbox"]["docker"]["dangerouslyAllowExternalBindSources"] is True
    assert local_entry["sandbox"]["docker"]["binds"] == [f"{work_dir}:/testbed:rw"]
    assert "PYTHONPATH" not in local_entry["sandbox"]["docker"]["env"]
    assert local_entry["sandbox"]["docker"]["env"]["PYTEST_ADDOPTS"] == "-o cache_dir=/tmp/swe-runner-pytest-cache"
    assert local_entry["sandbox"]["docker"]["env"]["HYPOTHESIS_STORAGE_DIRECTORY"] == "/tmp/swe-runner-hypothesis"
    setup_command = local_entry["sandbox"]["docker"]["setupCommand"]
    assert setup_command == (
        "mkdir -p /tmp/swe-runner-pytest-cache /tmp/swe-runner-hypothesis/examples "
        "&& git config --global --add safe.directory /testbed || true"
    )
    assert [
        "docker",
        "ps",
        "-aq",
        "--filter",
        "label=openclaw.sessionKey=agent:django__django-13448:main",
    ] in commands
    assert ["docker", "rm", "-f", "stale-container"] in commands


def test_prepare_writes_skill_agents_when_skill_enabled(tmp_path: Path) -> None:
    base_config = tmp_path / "base-openclaw.json"
    base_config.write_text('{"agents": {"list": [{"id": "main"}]}}', encoding="utf-8")
    workspace_root = tmp_path / "workspace"
    openclaw_workspace_root = workspace_root / "openclaw-workspace"
    work_dir = workspace_root / "repo"
    link_root = tmp_path / "home"
    link_root.mkdir()

    def fake_prepare_workspace(*args: object, **kwargs: object) -> Path:
        work_dir.mkdir(parents=True)
        return work_dir

    def fake_run(cmd: list[str], **kwargs: object) -> CommandResult:
        if cmd[:5] == ["openclaw", "--profile", cmd[2], "sandbox", "explain"]:
            return CommandResult(
                args=tuple(cmd),
                returncode=0,
                stdout=json.dumps({"sandbox": {"workspaceRoot": str(openclaw_workspace_root)}}),
                stderr="",
            )
        return CommandResult(args=tuple(cmd), returncode=0, stdout="", stderr="")

    agent = OpenClawAdapter(base_config_path=base_config, profile_link_root=link_root)
    with (
        patch("swe_runner.agents.openclaw.adapter.default_workspace_root", return_value=workspace_root),
        patch("swe_runner.agents.openclaw.adapter.prepare_workspace_from_image", side_effect=fake_prepare_workspace),
        patch("swe_runner.agents.openclaw.adapter.get_git_revision", return_value="base-rev"),
        patch("swe_runner.agents.openclaw.adapter.build_openclaw_prompt", return_value="fix the bug"),
        patch("swe_runner.agents.openclaw.adapter.load_optional_builtin_skill_text", return_value="# skill only\n\nDo X"),
        patch("swe_runner.agents.openclaw.sandbox.run_command", side_effect=fake_run),
    ):
        prepared = agent.prepare(make_instance(), make_settings(tmp_path, use_skill=True))

    agents_path = openclaw_workspace_root / "AGENTS.md"
    assert prepared.metadata["openclaw_injection_mode"] == "skill"
    assert prepared.metadata["openclaw_agents_path"] == str(agents_path)
    agents_text = agents_path.read_text(encoding="utf-8")
    assert agents_text.startswith("# Repository Task Guidelines")
    assert "Work only inside `/testbed`." in agents_text
    assert agents_text.endswith("# skill only\n\nDo X\n")
    assert not (openclaw_workspace_root / "BOOTSTRAP.md").exists()
    assert not (openclaw_workspace_root / "skills").exists()


def test_prepare_passes_per_case_prompt_into_user_prompt_when_enabled(tmp_path: Path) -> None:
    base_config = tmp_path / "base-openclaw.json"
    base_config.write_text('{"agents": {"list": [{"id": "main"}]}}', encoding="utf-8")
    workspace_root = tmp_path / "workspace"
    openclaw_workspace_root = workspace_root / "openclaw-workspace"
    work_dir = workspace_root / "repo"
    link_root = tmp_path / "home"
    link_root.mkdir()

    def fake_prepare_workspace(*args: object, **kwargs: object) -> Path:
        work_dir.mkdir(parents=True)
        return work_dir

    def fake_run(cmd: list[str], **kwargs: object) -> CommandResult:
        if cmd[:5] == ["openclaw", "--profile", cmd[2], "sandbox", "explain"]:
            return CommandResult(
                args=tuple(cmd),
                returncode=0,
                stdout=json.dumps({"sandbox": {"workspaceRoot": str(openclaw_workspace_root)}}),
                stderr="",
            )
        return CommandResult(args=tuple(cmd), returncode=0, stdout="", stderr="")

    instance = make_instance()
    agent = OpenClawAdapter(base_config_path=base_config, profile_link_root=link_root)
    with (
        patch("swe_runner.agents.openclaw.adapter.default_workspace_root", return_value=workspace_root),
        patch("swe_runner.agents.openclaw.adapter.prepare_workspace_from_image", side_effect=fake_prepare_workspace),
        patch("swe_runner.agents.openclaw.adapter.get_git_revision", return_value="base-rev"),
        patch(
            "swe_runner.agents.openclaw.adapter.build_openclaw_prompt", return_value="fix the bug"
        ) as mock_build_prompt,
        patch("swe_runner.agents.openclaw.sandbox.run_command", side_effect=fake_run),
    ):
        prepared = agent.prepare(instance, make_settings(tmp_path, per_case_prompt=True))

    mock_build_prompt.assert_called_once_with(
        instance,
        use_per_case_prompt=True,
        prompts_dir=None,
    )
    agents_path = openclaw_workspace_root / "AGENTS.md"
    assert prepared.metadata["openclaw_injection_mode"] == "per-case-prompt"
    assert prepared.metadata["openclaw_agents_path"] == str(agents_path)
    agents_text = agents_path.read_text(encoding="utf-8")
    assert agents_text.startswith("# Repository Task Guidelines")
    assert "Work only inside `/testbed`." in agents_text
    assert "<task_guidance>" not in agents_text
    assert not (openclaw_workspace_root / "BOOTSTRAP.md").exists()


def test_prepare_continues_when_per_case_prompt_file_is_missing(tmp_path: Path) -> None:
    base_config = tmp_path / "base-openclaw.json"
    base_config.write_text('{"agents": {"list": [{"id": "main"}]}}', encoding="utf-8")
    workspace_root = tmp_path / "workspace"
    openclaw_workspace_root = workspace_root / "openclaw-workspace"
    work_dir = workspace_root / "repo"
    link_root = tmp_path / "home"
    link_root.mkdir()

    def fake_prepare_workspace(*args: object, **kwargs: object) -> Path:
        work_dir.mkdir(parents=True)
        return work_dir

    def fake_run(cmd: list[str], **kwargs: object) -> CommandResult:
        if cmd[:5] == ["openclaw", "--profile", cmd[2], "sandbox", "explain"]:
            return CommandResult(
                args=tuple(cmd),
                returncode=0,
                stdout=json.dumps({"sandbox": {"workspaceRoot": str(openclaw_workspace_root)}}),
                stderr="",
            )
        return CommandResult(args=tuple(cmd), returncode=0, stdout="", stderr="")

    agent = OpenClawAdapter(base_config_path=base_config, profile_link_root=link_root)
    with (
        patch("swe_runner.agents.openclaw.adapter.default_workspace_root", return_value=workspace_root),
        patch("swe_runner.agents.openclaw.adapter.prepare_workspace_from_image", side_effect=fake_prepare_workspace),
        patch("swe_runner.agents.openclaw.adapter.get_git_revision", return_value="base-rev"),
        patch("swe_runner.agents.openclaw.prompts.load_custom_prompt", return_value=None),
        patch("swe_runner.agents.openclaw.sandbox.run_command", side_effect=fake_run),
    ):
        prepared = agent.prepare(make_instance(), make_settings(tmp_path, per_case_prompt=True))

    assert prepared.metadata["openclaw_injection_mode"] == "per-case-prompt"
    assert "<task_guidance>" not in prepared.prompt


def test_run_invokes_local_client_with_profile_session_and_ignored_step_limit(tmp_path: Path) -> None:
    agent = OpenClawAdapter()
    prepared = MagicMock()
    prepared.prompt = "fix the bug"
    prepared.timeout = 1800
    prepared.max_turns = 200
    prepared.instance.instance_id = "case-1"
    prepared.metadata = {
        "agent_id": "swebench",
        "session_id": "session-1",
        "openclaw_profile": "swebench-case-1",
    }

    with patch("swe_runner.agents.openclaw.adapter.OpenClawClient") as mock_client_cls:
        mock_client = mock_client_cls.return_value
        mock_client.run_prompt.return_value.returncode = 0
        mock_client.run_prompt.return_value.raw_output = "done"
        mock_client.run_prompt.return_value.duration_seconds = 1.25
        mock_client.run_prompt.return_value.error = None

        result = agent.run(prepared)

    mock_client_cls.assert_called_once_with(profile="swebench-case-1", agent_id="swebench", cli_path="openclaw")
    mock_client.run_prompt.assert_called_once_with(
        "fix the bug",
        session_id="session-1",
        timeout=1800,
        max_steps=200,
    )
    assert result.success is True
    assert result.raw_output == "done"
    assert result.metadata["openclaw_profile"] == "swebench-case-1"
    assert result.metadata["openclaw_returncode"] == "0"


def test_run_records_tokenless_evidence_for_tokenless_runs(tmp_path: Path) -> None:
    output_dir = tmp_path / "run"
    profile_dir = output_dir / "openclaw-profiles" / "case-1"
    session_dir = profile_dir / "agents" / "case-1" / "sessions"
    session_dir.mkdir(parents=True)
    tokenless_extension = profile_dir / "extensions" / "tokenless"
    tokenless_extension.mkdir(parents=True)
    (tokenless_extension / "openclaw.plugin.json").write_text('{"id":"tokenless"}', encoding="utf-8")
    workspace_root = tmp_path / "workspace" / "openclaw-workspace"
    tokenless_bin_dir = workspace_root / ".runner" / "tokenless" / "bin"
    tokenless_bin_dir.mkdir(parents=True)
    (tokenless_bin_dir / "rtk").write_text("#!/bin/sh\n", encoding="utf-8")
    (tokenless_bin_dir / "tokenless").write_text("#!/bin/sh\n", encoding="utf-8")
    (workspace_root / ".runner" / "tokenless" / "injection.json").write_text(
        json.dumps(
            {
                "schema_version": 1,
                "binaries": [
                    {
                        "name": "rtk",
                        "source": "/home/user/.local/bin/rtk",
                        "source_is_symlink": True,
                        "source_realpath": "/home/user/.local/libexec/anolisa/tokenless/rtk",
                        "copied": str(tokenless_bin_dir / "rtk"),
                        "copied_is_symlink": False,
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    config_path = profile_dir / "openclaw.json"
    config_path.write_text(
        json.dumps({"plugins": {"entries": {"tokenless": {"enabled": True}}}}),
        encoding="utf-8",
    )
    (session_dir / "case-1-session.jsonl").write_text(
        "\n".join(
            [
                json.dumps({"type": "session", "id": "case-1-session"}),
                json.dumps(
                    {
                        "type": "message",
                        "message": {
                            "role": "assistant",
                            "content": [
                                {
                                    "type": "toolCall",
                                    "name": "exec",
                                    "arguments": {"command": "ls /testbed"},
                                }
                            ],
                        },
                    }
                ),
                json.dumps(
                    {
                        "type": "message",
                        "message": {
                            "role": "toolResult",
                            "toolName": "exec",
                            "details": {"status": "completed"},
                        },
                    }
                ),
            ]
        ),
        encoding="utf-8",
    )
    (session_dir / "case-1-session.trajectory.jsonl").write_text(
        json.dumps(
            {
                "type": "trace.metadata",
                "data": {
                    "plugins": {
                        "entries": [
                            {
                                "id": "tokenless",
                                "status": "loaded",
                                "activated": True,
                                "explicitlyEnabled": True,
                            }
                        ]
                    }
                },
            }
        ),
        encoding="utf-8",
    )

    agent = OpenClawAdapter()
    prepared = MagicMock()
    prepared.prompt = "fix the bug"
    prepared.timeout = 1800
    prepared.max_turns = 200
    prepared.instance.instance_id = "case-1"
    prepared.settings.output.output_dir = output_dir
    prepared.metadata = {
        "agent_id": "case-1",
        "session_id": "case-1-session",
        "openclaw_profile": "swebench-case-1",
        "openclaw_profile_dir": str(profile_dir),
        "openclaw_config_path": str(config_path),
        "openclaw_workspace_root": str(workspace_root),
        "openclaw_tokenless_requested": "true",
    }

    raw_output = "\n".join(
        [
            "[tokenless] OpenClaw plugin registered — active features: rtk-rewrite, tool-ready, response-compression",
            "[tokenless:rtk] rewrite: ls /testbed -> rtk ls /testbed",
        ]
    )
    with patch("swe_runner.agents.openclaw.adapter.OpenClawClient") as mock_client_cls:
        mock_client = mock_client_cls.return_value
        mock_client.run_prompt.return_value.returncode = 0
        mock_client.run_prompt.return_value.raw_output = raw_output
        mock_client.run_prompt.return_value.duration_seconds = 1.25
        mock_client.run_prompt.return_value.error = None

        result = agent.run(prepared)

    assert result.metadata["openclaw_tokenless_evidence_strong"] == "true"
    assert result.metadata["openclaw_tokenless_plugin_loaded"] == "true"
    assert result.metadata["openclaw_tokenless_hook_seen"] == "true"
    assert result.metadata["openclaw_tokenless_exec_tool_calls"] == "1"
    evidence_path = Path(result.metadata["openclaw_tokenless_evidence_path"])
    evidence = json.loads(evidence_path.read_text(encoding="utf-8"))
    assert evidence["strong"] is True
    assert evidence["reasons"] == {
        "config_enabled": True,
        "sandbox_binaries_present": True,
        "profile_extension_present": True,
        "plugin_loaded": True,
        "hook_seen": True,
        "exec_tool_calls": 1,
    }
    assert evidence["profile_extension"]["manifest_exists"] is True
    assert evidence["injection_manifest"]["content"]["binaries"][0]["source_is_symlink"] is True
    assert evidence["raw_output"]["sample_rtk_rewrite_lines"] == [
        "[tokenless:rtk] rewrite: ls /testbed -> rtk ls /testbed"
    ]
    assert evidence["session"]["exec_tool_call_count"] == 1
    assert evidence["trajectory"]["tokenless_status"] == "loaded"


def test_run_writes_error_log_for_nonzero_openclaw_exit(tmp_path: Path) -> None:
    agent = OpenClawAdapter()
    prepared = MagicMock()
    prepared.prompt = "fix the bug"
    prepared.timeout = 1800
    prepared.max_turns = 200
    prepared.instance.instance_id = "case-1"
    prepared.settings.output.output_dir = tmp_path / "run"
    prepared.metadata = {
        "agent_id": "case-1",
        "session_id": "case-1-session",
        "openclaw_profile": "swebench-case-1",
    }

    with patch("swe_runner.agents.openclaw.adapter.OpenClawClient") as mock_client_cls:
        mock_client = mock_client_cls.return_value
        mock_client.run_prompt.return_value.returncode = 2
        mock_client.run_prompt.return_value.raw_output = "stdout\nstderr\n"
        mock_client.run_prompt.return_value.duration_seconds = 1.25
        mock_client.run_prompt.return_value.error = "stdout\nstderr\n"

        result = agent.run(prepared)

    assert result.success is False
    assert result.metadata["openclaw_returncode"] == "2"
    error_log = Path(result.metadata["openclaw_error_log"])
    assert error_log == tmp_path / "run" / "openclaw-errors" / "case-1.log"
    assert "session_id=case-1-session" in error_log.read_text(encoding="utf-8")
    assert "stdout\nstderr" in error_log.read_text(encoding="utf-8")
    assert result.error == f"OpenClaw local exited with return code 2; see {error_log}"


def test_local_client_builds_openclaw_command() -> None:
    with patch("swe_runner.agents.openclaw.client.run_command") as mock_run:
        mock_run.return_value = CommandResult(
            args=("openclaw",),
            returncode=0,
            stdout=json.dumps({"payload": {"rawOutput": "local output"}}),
            stderr="",
        )

        outcome = OpenClawClient(profile="profile-1", agent_id="swebench").run_prompt(
            "fix",
            session_id="session-1",
            timeout=1200,
            max_steps=999,
        )

    assert outcome.raw_output == json.dumps({"payload": {"rawOutput": "local output"}})
    assert outcome.returncode == 0
    assert mock_run.call_args.args[0] == [
        "openclaw",
        "--profile",
        "profile-1",
        "agent",
        "--local",
        "--json",
        "--agent",
        "swebench",
        "--session-id",
        "session-1",
        "--message",
        "fix",
        "--timeout",
        "1200",
    ]


def test_build_openclaw_session_id_is_cli_friendly() -> None:
    session_id = build_openclaw_session_id("django/django#13448")
    assert session_id.startswith("django-django-13448-")
    assert ":" not in session_id


def test_build_openclaw_agent_id_uses_case_id() -> None:
    assert build_openclaw_agent_id("django__django-13448") == "django__django-13448"

    agent_id = build_openclaw_agent_id("django/django#13448")
    assert agent_id == "django-django-13448"
    assert len(agent_id) <= 63
    assert ":" not in agent_id


def test_build_openclaw_agent_scope_key_matches_local_agent_scope() -> None:
    assert build_openclaw_agent_scope_key("django__django-13448") == "agent:django__django-13448:main"


def test_openclaw_adapter_registered() -> None:
    agent = get_agent("openclaw")
    assert isinstance(agent, OpenClawAdapter)
    assert agent.name == "openclaw"
