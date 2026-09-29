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

"""Unit tests for ensure_user_session_persistent() linger check."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path
from unittest.mock import patch, MagicMock

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))

from ce_runner.infra import ensure_user_session_persistent  # noqa: E402


class TestEnsureUserSessionPersistent:
    def test_noop_when_linger_already_enabled(self):
        result = MagicMock(returncode=0, stdout="Linger=yes\n")
        with patch.dict("os.environ", {"USER": "root"}, clear=False), \
             patch("ce_runner.infra.subprocess.run", return_value=result) as mock_run, \
             patch("ce_runner.infra.subprocess.check_output"):
            ensure_user_session_persistent()
        mock_run.assert_called_once()
        args = mock_run.call_args[0][0]
        assert "show-user" in args

    def test_enables_linger_when_not_set(self):
        query_result = MagicMock(returncode=0, stdout="Linger=no\n")
        enable_result = MagicMock(returncode=0)
        with patch.dict("os.environ", {"USER": "testuser"}, clear=False), \
             patch("ce_runner.infra.subprocess.run",
                   side_effect=[query_result, enable_result]) as mock_run, \
             patch("ce_runner.infra.subprocess.check_output"):
            ensure_user_session_persistent()
        assert mock_run.call_count == 2
        enable_args = mock_run.call_args_list[1][0][0]
        assert enable_args == ["loginctl", "enable-linger", "testuser"]

    def test_enables_linger_when_query_fails_nonzero(self):
        query_result = MagicMock(returncode=1, stdout="")
        enable_result = MagicMock(returncode=0)
        with patch.dict("os.environ", {"USER": "root"}, clear=False), \
             patch("ce_runner.infra.subprocess.run",
                   side_effect=[query_result, enable_result]) as mock_run, \
             patch("ce_runner.infra.subprocess.check_output"):
            ensure_user_session_persistent()
        assert mock_run.call_count == 2

    def test_warns_when_query_raises_exception(self):
        with patch.dict("os.environ", {"USER": "root"}, clear=False), \
             patch("ce_runner.infra.subprocess.run",
                   side_effect=FileNotFoundError("loginctl not found")), \
             patch("ce_runner.infra.log") as mock_log:
            ensure_user_session_persistent()
        mock_log.assert_called_once()
        assert "Could not query linger" in mock_log.call_args[0][0]

    def test_warns_when_enable_fails(self):
        query_result = MagicMock(returncode=0, stdout="Linger=no\n")
        with patch.dict("os.environ", {"USER": "root"}, clear=False), \
             patch("ce_runner.infra.subprocess.run",
                   side_effect=[query_result,
                                subprocess.CalledProcessError(1, "loginctl")]), \
             patch("ce_runner.infra.log") as mock_log:
            ensure_user_session_persistent()
        assert any("Could not enable linger" in str(c) for c in mock_log.call_args_list)

    def test_falls_back_to_id_command_when_user_env_missing(self):
        query_result = MagicMock(returncode=0, stdout="Linger=yes\n")
        with patch.dict("os.environ", {}, clear=True), \
             patch("ce_runner.infra.subprocess.check_output",
                   return_value="fallback\n") as mock_co, \
             patch("ce_runner.infra.subprocess.run",
                   return_value=query_result) as mock_run:
            ensure_user_session_persistent()
        mock_co.assert_called_once_with(["id", "-un"], text=True)
        show_args = mock_run.call_args[0][0]
        assert "fallback" in show_args

    def test_returns_silently_when_user_lookup_fails(self):
        with patch.dict("os.environ", {}, clear=True), \
             patch("ce_runner.infra.subprocess.check_output",
                   side_effect=FileNotFoundError("no id")):
            ensure_user_session_persistent()
