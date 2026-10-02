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

"""Git command helpers for runner workspaces."""

from __future__ import annotations

import subprocess
from collections.abc import Sequence
from pathlib import Path

from swe_runner.common.commands import CommandResult, run_command


class GitCommandError(RuntimeError):
    """Raised when a git command cannot be executed successfully."""

    def __init__(self, command: Sequence[str], *, returncode: int | None = None, output: str = "") -> None:
        self.command = tuple(command)
        self.returncode = returncode
        self.output = output
        details = f"git command failed: {' '.join(self.command)}"
        if returncode is not None:
            details = f"{details} (exit {returncode})"
        super().__init__(details)


def _git_command(
    args: Sequence[str],
    *,
    work_dir: Path | None = None,
    config: Sequence[tuple[str, str]] = (),
) -> tuple[str, ...]:
    command: list[str] = ["git"]
    if work_dir is not None:
        command.extend(["-C", str(work_dir)])
    for key, value in config:
        command.extend(["-c", f"{key}={value}"])
    command.extend(args)
    return tuple(command)


def run_git(
    args: Sequence[str],
    *,
    work_dir: Path | None = None,
    config: Sequence[tuple[str, str]] = (),
    timeout: float | None = None,
    check: bool = False,
) -> CommandResult:
    """Run a git command with normalized execution and optional error mapping."""
    command = _git_command(args, work_dir=work_dir, config=config)
    try:
        result = run_command(command, timeout=timeout)
    except (OSError, subprocess.SubprocessError) as exc:
        raise GitCommandError(command, output=str(exc)) from exc
    if check and result.returncode != 0:
        raise GitCommandError(command, returncode=result.returncode, output=result.output)
    return result


def _successful_stdout(
    args: Sequence[str],
    *,
    work_dir: Path,
    timeout: float | None = None,
    empty_as_none: bool = True,
) -> str | None:
    try:
        result = run_git(args, work_dir=work_dir, timeout=timeout)
    except GitCommandError:
        return None
    if result.returncode != 0:
        return None
    stdout = result.stdout.strip()
    if empty_as_none and not stdout:
        return None
    return stdout


def get_git_revision(work_dir: Path | None, *, timeout: float | None = None) -> str | None:
    """Return the current git revision for *work_dir*, or ``None`` if unavailable."""
    if work_dir is None:
        return None
    return _successful_stdout(["rev-parse", "HEAD"], work_dir=work_dir, timeout=timeout)


def get_git_status_porcelain(work_dir: Path, *, timeout: float | None = 5) -> str | None:
    """Return ``git status --porcelain`` output, or ``None`` when git is unavailable."""
    return _successful_stdout(["status", "--porcelain"], work_dir=work_dir, timeout=timeout, empty_as_none=False)


def stage_all(work_dir: Path) -> None:
    """Stage all tracked and untracked changes in *work_dir*."""
    run_git(["add", "-A"], work_dir=work_dir, check=True)


def cached_diff(
    work_dir: Path,
    *,
    base_revision: str | None = None,
    pathspecs: Sequence[str] = (),
) -> str:
    """Return the cached text diff for *work_dir*."""
    args = ["diff", "--cached", "--no-color", "--no-ext-diff"]
    if base_revision:
        args.append(base_revision)
    args.extend(["--", ".", *pathspecs])
    result = run_git(args, work_dir=work_dir, config=(("core.quotepath", "off"),), check=True)
    return result.stdout
