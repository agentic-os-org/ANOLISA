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

"""Tests for OpenClaw local sandbox management."""

from __future__ import annotations

import json
import os
from pathlib import Path
from unittest.mock import patch

import pytest

from swe_runner.agents.openclaw.sandbox import (
    OpenClawSandboxManager,
    OpenClawSandboxSpec,
    build_openclaw_agent_scope_key,
)
from swe_runner.common.commands import CommandResult


def _spec(tmp_path: Path, *, agent_id: str = "django__django-13448") -> OpenClawSandboxSpec:
    workspace = tmp_path / "workspace"
    testbed_dir = workspace / "repo"
    testbed_dir.mkdir(parents=True)
    return OpenClawSandboxSpec(
        agent_id=agent_id,
        image_name="swebench/django-13448:latest",
        workspace_root=workspace / "openclaw-workspace",
        testbed_dir=testbed_dir,
    )


def _tokenless_extensions_dir(tmp_path: Path) -> Path:
    extensions_dir = tmp_path / "host-openclaw-extensions"
    tokenless_extension = extensions_dir / "tokenless"
    tokenless_extension.mkdir(parents=True)
    (tokenless_extension / "openclaw.plugin.json").write_text('{"id":"tokenless"}', encoding="utf-8")
    return extensions_dir


def _completed(cmd: list[str], stdout: str = "", stderr: str = "", returncode: int = 0) -> CommandResult:
    return CommandResult(args=tuple(cmd), returncode=returncode, stdout=stdout, stderr=stderr)


def test_sandbox_manager_writes_single_case_agent_config(tmp_path: Path) -> None:
    config_path = tmp_path / "openclaw.json"
    config_path.write_text(
        json.dumps(
            {
                "agents": {
                    "defaults": {
                        "thinkingDefault": "low",
                        "params": {"extra_body": {"enable_thinking": True, "custom": "keep-me"}},
                    },
                    "list": [{"id": "main", "default": True}],
                }
            }
        ),
        encoding="utf-8",
    )
    spec = _spec(tmp_path)
    commands: list[list[str]] = []

    def fake_run(cmd: list[str], **kwargs: object) -> CommandResult:
        commands.append(cmd)
        if cmd[:2] == ["docker", "ps"]:
            return _completed(cmd)
        if cmd[:5] == ["openclaw", "--profile", "profile-1", "sandbox", "explain"]:
            return _completed(cmd, stdout=json.dumps({"sandbox": {"workspaceRoot": str(spec.workspace_root)}}))
        return _completed(cmd)

    manager = OpenClawSandboxManager(config_path=config_path, profile="profile-1", cli_path="openclaw")

    with patch("swe_runner.agents.openclaw.sandbox.run_command", side_effect=fake_run):
        manager.configure(spec)

    data = json.loads(config_path.read_text(encoding="utf-8"))
    assert "thinkingDefault" not in data["agents"]["defaults"]
    assert data["agents"]["defaults"]["params"] == {
        "temperature": 0,
        "top_p": 1,
        "seed": 42,
        "extra_body": {"custom": "keep-me"},
    }
    assert data["agents"]["defaults"]["skipBootstrap"] is True
    assert "reasoningDefault" not in data["agents"]["defaults"]

    entry = next(item for item in data["agents"]["list"] if item["id"] == spec.agent_id)
    assert entry["name"] == spec.agent_id
    assert entry["workspace"] == str(spec.workspace_root)
    assert entry["sandbox"]["backend"] == "docker"
    assert entry["sandbox"]["scope"] == "agent"
    assert entry["sandbox"]["workspaceAccess"] == "rw"
    assert entry["sandbox"]["workspaceRoot"] == str(spec.workspace_root)
    assert entry["sandbox"]["docker"]["image"] == "swebench/django-13448:latest"
    assert "user" not in entry["sandbox"]["docker"]
    assert entry["sandbox"]["docker"]["dangerouslyAllowExternalBindSources"] is True
    assert entry["sandbox"]["docker"]["workdir"] == "/workspace"
    assert entry["sandbox"]["docker"]["binds"] == [f"{spec.testbed_dir}:/testbed:rw"]
    assert entry["sandbox"]["docker"]["env"]["PATH"].startswith("/opt/miniconda3/envs/testbed/bin:")
    assert entry["sandbox"]["docker"]["env"]["VIRTUAL_ENV"] == "/opt/miniconda3/envs/testbed"
    assert "PYTHONPATH" not in entry["sandbox"]["docker"]["env"]
    assert entry["sandbox"]["docker"]["env"]["PYTEST_ADDOPTS"] == "-o cache_dir=/tmp/swe-runner-pytest-cache"
    assert entry["sandbox"]["docker"]["env"]["HYPOTHESIS_STORAGE_DIRECTORY"] == "/tmp/swe-runner-hypothesis"
    assert entry["sandbox"]["docker"]["setupCommand"] == (
        "mkdir -p /tmp/swe-runner-pytest-cache /tmp/swe-runner-hypothesis/examples "
        "&& git config --global --add safe.directory /testbed || true"
    )
    assert [
        "openclaw",
        "--profile",
        "profile-1",
        "sandbox",
        "recreate",
        "--agent",
        spec.agent_id,
        "--force",
    ] in commands
    assert [
        "docker",
        "ps",
        "-aq",
        "--filter",
        f"label=openclaw.sessionKey=agent:{spec.agent_id}:main",
    ] in commands
    assert [
        "openclaw",
        "--profile",
        "profile-1",
        "sandbox",
        "explain",
        "--agent",
        spec.agent_id,
        "--json",
    ] in commands


def test_sandbox_manager_writes_agents_text_and_ignores_skill_extra_dirs(tmp_path: Path) -> None:
    config_path = tmp_path / "openclaw.json"
    skill_root = tmp_path / "my_skill"
    skill_dir = skill_root / "swe-bench-patch-generation"
    skill_dir.mkdir(parents=True)
    (skill_dir / "SKILL.md").write_text("# injected skill\n", encoding="utf-8")
    config_path.write_text(
        json.dumps(
            {
                "skills": {"load": {"extraDirs": [str(skill_root)]}},
                "agents": {"list": [{"id": "main", "default": True}]},
            }
        ),
        encoding="utf-8",
    )
    spec = _spec(tmp_path)
    spec = OpenClawSandboxSpec(
        agent_id=spec.agent_id,
        image_name=spec.image_name,
        workspace_root=spec.workspace_root,
        testbed_dir=spec.testbed_dir,
        agents_text="# injected skill\n\nFollow this.",
    )

    def fake_run(cmd: list[str], **kwargs: object) -> CommandResult:
        if cmd[:5] == ["openclaw", "--profile", "profile-1", "sandbox", "explain"]:
            return _completed(cmd, stdout=json.dumps({"sandbox": {"workspaceRoot": str(spec.workspace_root)}}))
        return _completed(cmd)

    manager = OpenClawSandboxManager(config_path=config_path, profile="profile-1", cli_path="openclaw")

    with patch("swe_runner.agents.openclaw.sandbox.run_command", side_effect=fake_run):
        manager.configure(spec)

    assert (spec.workspace_root / "AGENTS.md").read_text(encoding="utf-8") == "# injected skill\n\nFollow this.\n"
    assert not (spec.workspace_root / "BOOTSTRAP.md").exists()
    data = json.loads(config_path.read_text(encoding="utf-8"))
    assert data["agents"]["defaults"]["skipBootstrap"] is True
    injected_skill = spec.workspace_root / "skills" / "swe-bench-patch-generation" / "SKILL.md"
    assert not injected_skill.exists()


def test_sandbox_manager_can_inject_tokenless_binaries(tmp_path: Path) -> None:
    config_path = tmp_path / "openclaw.json"
    config_path.write_text('{"agents":{"list":[{"id":"main","default":true}]}}', encoding="utf-8")
    tokenless_bin = tmp_path / "host-tokenless-bin"
    tokenless_bin.mkdir()
    (tokenless_bin / "real-rtk").write_text("#!/bin/sh\n", encoding="utf-8")
    (tokenless_bin / "rtk").symlink_to(tokenless_bin / "real-rtk")
    (tokenless_bin / "tokenless").write_text("#!/bin/sh\n", encoding="utf-8")
    extensions_dir = _tokenless_extensions_dir(tmp_path)
    spec = _spec(tmp_path)

    def fake_run(cmd: list[str], **kwargs: object) -> CommandResult:
        if cmd[:5] == ["openclaw", "--profile", "profile-1", "sandbox", "explain"]:
            return _completed(cmd, stdout=json.dumps({"sandbox": {"workspaceRoot": str(spec.workspace_root)}}))
        return _completed(cmd)

    manager = OpenClawSandboxManager(config_path=config_path, profile="profile-1", cli_path="openclaw", tokenless=True)

    with (
        patch("swe_runner.agents.openclaw.sandbox._HOST_TOKENLESS_BIN_DIR", tokenless_bin),
        patch("swe_runner.agents.openclaw.sandbox._HOST_OPENCLAW_EXTENSIONS_DIR", extensions_dir),
        patch("swe_runner.agents.openclaw.sandbox.run_command", side_effect=fake_run),
    ):
        manager.configure(spec)

    data = json.loads(config_path.read_text(encoding="utf-8"))
    entry = next(item for item in data["agents"]["list"] if item["id"] == spec.agent_id)
    assert entry["sandbox"]["docker"]["env"]["PATH"].startswith(
        "/workspace/.runner/tokenless/bin:/opt/miniconda3/envs/testbed/bin:"
    )
    assert "allow" not in data["plugins"]
    assert data["plugins"]["entries"]["tokenless"]["enabled"] is True
    injected_rtk = spec.workspace_root / ".runner" / "tokenless" / "bin" / "rtk"
    assert injected_rtk.is_file()
    assert not injected_rtk.is_symlink()
    assert (spec.workspace_root / ".runner" / "tokenless" / "bin" / "tokenless").is_file()
    extension_link = config_path.parent / "extensions" / "tokenless"
    assert extension_link.is_symlink()
    assert extension_link.resolve() == (extensions_dir / "tokenless").resolve()
    injection_manifest = json.loads(
        (spec.workspace_root / ".runner" / "tokenless" / "injection.json").read_text(encoding="utf-8")
    )
    rtk_record = next(item for item in injection_manifest["binaries"] if item["name"] == "rtk")
    assert rtk_record["source_is_symlink"] is True
    assert rtk_record["copied_is_symlink"] is False


def test_sandbox_manager_adds_tokenless_to_existing_plugin_allowlist(tmp_path: Path) -> None:
    config_path = tmp_path / "openclaw.json"
    config_path.write_text(
        json.dumps(
            {
                "agents": {"list": [{"id": "main", "default": True}]},
                "plugins": {
                    "allow": ["memory-core"],
                    "entries": {
                        "tokenless": {
                            "enabled": False,
                            "config": {"verbose": True},
                        }
                    },
                },
            }
        ),
        encoding="utf-8",
    )
    tokenless_bin = tmp_path / "host-tokenless-bin"
    tokenless_bin.mkdir()
    (tokenless_bin / "rtk").write_text("#!/bin/sh\n", encoding="utf-8")
    (tokenless_bin / "tokenless").write_text("#!/bin/sh\n", encoding="utf-8")
    extensions_dir = _tokenless_extensions_dir(tmp_path)
    spec = _spec(tmp_path)

    def fake_run(cmd: list[str], **kwargs: object) -> CommandResult:
        if cmd[:5] == ["openclaw", "--profile", "profile-1", "sandbox", "explain"]:
            return _completed(cmd, stdout=json.dumps({"sandbox": {"workspaceRoot": str(spec.workspace_root)}}))
        return _completed(cmd)

    manager = OpenClawSandboxManager(config_path=config_path, profile="profile-1", cli_path="openclaw", tokenless=True)

    with (
        patch("swe_runner.agents.openclaw.sandbox._HOST_TOKENLESS_BIN_DIR", tokenless_bin),
        patch("swe_runner.agents.openclaw.sandbox._HOST_OPENCLAW_EXTENSIONS_DIR", extensions_dir),
        patch("swe_runner.agents.openclaw.sandbox.run_command", side_effect=fake_run),
    ):
        manager.configure(spec)

    data = json.loads(config_path.read_text(encoding="utf-8"))
    assert data["plugins"]["allow"] == ["memory-core", "tokenless"]
    assert data["plugins"]["entries"]["tokenless"] == {
        "enabled": True,
        "config": {"verbose": True},
    }


def test_sandbox_manager_detects_tokenless_binaries_from_path(tmp_path: Path) -> None:
    config_path = tmp_path / "openclaw.json"
    config_path.write_text('{"agents":{"list":[{"id":"main","default":true}]}}', encoding="utf-8")
    rtk_bin = tmp_path / "rtk-bin"
    tokenless_bin = tmp_path / "tokenless-bin"
    rtk_bin.mkdir()
    tokenless_bin.mkdir()
    (rtk_bin / "rtk").write_text("#!/bin/sh\n", encoding="utf-8")
    (tokenless_bin / "tokenless").write_text("#!/bin/sh\n", encoding="utf-8")
    (rtk_bin / "rtk").chmod(0o755)
    (tokenless_bin / "tokenless").chmod(0o755)
    extensions_dir = _tokenless_extensions_dir(tmp_path)
    spec = _spec(tmp_path)

    def fake_run(cmd: list[str], **kwargs: object) -> CommandResult:
        if cmd[:5] == ["openclaw", "--profile", "profile-1", "sandbox", "explain"]:
            return _completed(cmd, stdout=json.dumps({"sandbox": {"workspaceRoot": str(spec.workspace_root)}}))
        return _completed(cmd)

    manager = OpenClawSandboxManager(config_path=config_path, profile="profile-1", cli_path="openclaw", tokenless=True)

    with (
        patch.dict(os.environ, {"PATH": f"{rtk_bin}{os.pathsep}{tokenless_bin}"}, clear=True),
        patch("swe_runner.agents.openclaw.sandbox._HOST_TOKENLESS_BIN_DIR", tmp_path / "missing-tokenless-bin"),
        patch("swe_runner.agents.openclaw.sandbox._HOST_TOKENLESS_EXTRA_BIN_DIRS", ()),
        patch("swe_runner.agents.openclaw.sandbox._HOST_OPENCLAW_EXTENSIONS_DIR", extensions_dir),
        patch("swe_runner.agents.openclaw.sandbox.run_command", side_effect=fake_run),
    ):
        manager.configure(spec)

    assert (spec.workspace_root / ".runner" / "tokenless" / "bin" / "rtk").is_file()
    assert (spec.workspace_root / ".runner" / "tokenless" / "bin" / "tokenless").is_file()


def test_remove_stale_agent_containers_removes_labelled_containers(tmp_path: Path) -> None:
    config_path = tmp_path / "openclaw.json"
    manager = OpenClawSandboxManager(config_path=config_path, profile="profile-1", cli_path="openclaw")
    commands: list[list[str]] = []

    def fake_run(cmd: list[str], **kwargs: object) -> CommandResult:
        commands.append(cmd)
        if cmd[:2] == ["docker", "ps"]:
            return _completed(cmd, stdout="container-a\ncontainer-b\n")
        return _completed(cmd)

    with patch("swe_runner.agents.openclaw.sandbox.run_command", side_effect=fake_run):
        manager.remove_stale_agent_containers("django__django-13448")

    assert [
        "docker",
        "ps",
        "-aq",
        "--filter",
        "label=openclaw.sessionKey=agent:django__django-13448:main",
    ] in commands
    assert ["docker", "rm", "-f", "container-a", "container-b"] in commands


def test_sandbox_manager_rejects_wrong_explained_workspace(tmp_path: Path) -> None:
    config_path = tmp_path / "openclaw.json"
    config_path.write_text('{"agents":{"list":[{"id":"main","default":true}]}}', encoding="utf-8")
    spec = _spec(tmp_path)

    def fake_run(cmd: list[str], **kwargs: object) -> CommandResult:
        if cmd[:5] == ["openclaw", "--profile", "profile-1", "sandbox", "explain"]:
            return _completed(cmd, stdout=json.dumps({"sandbox": {"workspaceRoot": str(tmp_path / "wrong")}}))
        return _completed(cmd)

    manager = OpenClawSandboxManager(config_path=config_path, profile="profile-1", cli_path="openclaw")

    with (
        patch("swe_runner.agents.openclaw.sandbox.run_command", side_effect=fake_run),
        pytest.raises(RuntimeError, match="does not reference the expected workspaceRoot"),
    ):
        manager.configure(spec)


def test_build_openclaw_agent_scope_key_matches_local_agent_scope() -> None:
    assert build_openclaw_agent_scope_key("django__django-13448") == "agent:django__django-13448:main"
