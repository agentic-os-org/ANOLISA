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

"""Tests for shared command execution helpers."""

from __future__ import annotations

import subprocess
from pathlib import Path
from unittest.mock import MagicMock, patch

from swe_runner.common.commands import run_command


def test_run_command_normalizes_text_output() -> None:
    with patch("swe_runner.common.commands.subprocess.run") as mock_run:
        mock_run.return_value = MagicMock(stdout="out", stderr=None, returncode=0)

        result = run_command(["echo", "hello"], cwd=Path("/tmp/work"), timeout=5)

    assert result.args == ("echo", "hello")
    assert result.returncode == 0
    assert result.stdout == "out"
    assert result.stderr == ""
    assert result.output == "out"
    mock_run.assert_called_once_with(
        ["echo", "hello"],
        cwd="/tmp/work",
        capture_output=True,
        text=True,
        timeout=5,
        check=False,
        encoding="utf-8",
        errors="replace",
    )


def test_run_command_preserves_subprocess_exceptions() -> None:
    with patch(
        "swe_runner.common.commands.subprocess.run",
        side_effect=subprocess.TimeoutExpired(cmd=["sleep"], timeout=1),
    ):
        try:
            run_command(["sleep"], timeout=1)
        except subprocess.TimeoutExpired as exc:
            assert exc.timeout == 1
        else:
            raise AssertionError("Expected TimeoutExpired")
