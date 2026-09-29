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
from unittest.mock import patch

import pytest

from swe_runner.common.commands import CommandResult
from swe_runner.run.workspace.git import (
    GitCommandError,
    cached_diff,
    get_git_revision,
    get_git_status_porcelain,
    run_git,
    stage_all,
)


def test_run_git_adds_worktree_and_config_options() -> None:
    with patch("swe_runner.run.workspace.git.run_command") as mock_run:
        mock_run.return_value = CommandResult(args=(), returncode=0, stdout="ok\n", stderr="")

        result = run_git(
            ["diff"],
            work_dir=Path("/tmp/repo"),
            config=(("core.quotepath", "off"),),
            timeout=5,
        )

    mock_run.assert_called_once_with(
        ("git", "-C", "/tmp/repo", "-c", "core.quotepath=off", "diff"),
        timeout=5,
    )
    assert result.stdout == "ok\n"


def test_run_git_maps_command_failure_when_checked() -> None:
    with patch("swe_runner.run.workspace.git.run_command") as mock_run:
        mock_run.return_value = CommandResult(
            args=("git", "status"),
            returncode=128,
            stdout="",
            stderr="fatal: not a git repository\n",
        )

        with pytest.raises(GitCommandError) as exc_info:
            run_git(["status"], check=True)

    assert exc_info.value.returncode == 128
    assert "fatal: not a git repository" in exc_info.value.output


def test_get_git_revision_returns_none_for_empty_or_failed_output() -> None:
    with patch("swe_runner.run.workspace.git.run_command") as mock_run:
        mock_run.return_value = CommandResult(args=(), returncode=0, stdout="\n", stderr="")

        assert get_git_revision(Path("/tmp/repo")) is None

    with patch("swe_runner.run.workspace.git.run_command", side_effect=OSError("git missing")):
        assert get_git_revision(Path("/tmp/repo")) is None


def test_get_git_status_porcelain_preserves_clean_output() -> None:
    with patch("swe_runner.run.workspace.git.run_command") as mock_run:
        mock_run.return_value = CommandResult(args=(), returncode=0, stdout="", stderr="")

        assert get_git_status_porcelain(Path("/tmp/repo")) == ""


def test_stage_all_and_cached_diff_use_high_level_git_commands() -> None:
    calls: list[tuple[str, ...]] = []

    def fake_run_git(args: list[str], **kwargs: object) -> CommandResult:
        calls.append(tuple(args))
        if args[0] == "diff":
            return CommandResult(args=("git", *args), returncode=0, stdout="diff text", stderr="")
        return CommandResult(args=("git", *args), returncode=0, stdout="", stderr="")

    with patch("swe_runner.run.workspace.git.run_git", side_effect=fake_run_git):
        stage_all(Path("/tmp/repo"))
        result = cached_diff(Path("/tmp/repo"), base_revision="base", pathspecs=(":(exclude)tests/**",))

    assert calls == [
        ("add", "-A"),
        ("diff", "--cached", "--no-color", "--no-ext-diff", "base", "--", ".", ":(exclude)tests/**"),
    ]
    assert result == "diff text"
