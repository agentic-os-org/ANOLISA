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

"""Tests for environment pre-flight checks."""

from __future__ import annotations

from unittest.mock import patch

import pytest
from pytest_mock import MockerFixture

from swe_runner.agents import (
    AGENT_DESCRIPTOR_REGISTRY,
    AgentDescriptor,
    AgentEnvironmentError,
    register_agent_descriptor,
)
from swe_runner.agents.env_checks import check_agent_binary, check_agent_environment


class TestCheckAgentBinary:
    def test_descriptor_declares_required_binary(self) -> None:
        register_agent_descriptor(AgentDescriptor(name="binary-test-agent", required_binaries=("binary-test-cli",)))
        try:
            with patch("shutil.which", return_value="/usr/bin/binary-test-cli") as mock_which:
                check_agent_binary("binary-test-agent")

            mock_which.assert_called_once_with("binary-test-cli")
        finally:
            AGENT_DESCRIPTOR_REGISTRY.pop("binary-test-agent", None)

    def test_openclaw_requires_local_cli(self) -> None:
        with patch("shutil.which", return_value="/usr/bin/openclaw") as mock_which:
            check_agent_binary("openclaw")

        mock_which.assert_called_once_with("openclaw")

    def test_openclaw_missing_cli_raises(self) -> None:
        with (
            patch("shutil.which", return_value=None),
            pytest.raises(AgentEnvironmentError, match="Agent binary 'openclaw' not found"),
        ):
            check_agent_binary("openclaw")

    def test_unknown_agent_has_no_binary_check(self) -> None:
        with patch("shutil.which") as mock_which:
            check_agent_binary("unknown-agent-that-does-not-exist")

        mock_which.assert_not_called()


class TestCheckAgentEnvironment:
    """Tests for check_agent_environment function."""

    def test_dispatches_to_descriptor_check(self, mocker: MockerFixture) -> None:
        """Test that check_agent_environment dispatches to descriptor checks."""
        mocker.patch("swe_runner.agents.env_checks.check_docker_available")
        mocker.patch("swe_runner.agents.env_checks.check_disk_space")
        mocker.patch("swe_runner.agents.env_checks.check_agent_binary")

        mock_check = mocker.MagicMock()
        register_agent_descriptor(AgentDescriptor(name="test-agent-dispatch", environment_checks=(mock_check,)))

        try:
            check_agent_environment("test-agent-dispatch", extra_param="value")
            mock_check.assert_called_once_with(extra_param="value")
        finally:
            AGENT_DESCRIPTOR_REGISTRY.pop("test-agent-dispatch", None)

    def test_no_check_for_unknown_agent(self, mocker: MockerFixture) -> None:
        """Test that unknown agent returns silently after universal checks."""
        mocker.patch("swe_runner.agents.env_checks.check_docker_available")
        mocker.patch("swe_runner.agents.env_checks.check_disk_space")
        mocker.patch("swe_runner.agents.env_checks.check_agent_binary")

        check_agent_environment("unknown-agent-that-does-not-exist")

    def test_openclaw_has_no_gateway_specific_check(self, mocker: MockerFixture) -> None:
        """OpenClaw local mode only needs the shared Docker/disk/binary checks."""
        mocker.patch("swe_runner.agents.env_checks.check_docker_available")
        mocker.patch("swe_runner.agents.env_checks.check_disk_space")
        mock_binary = mocker.patch("swe_runner.agents.env_checks.check_agent_binary")

        check_agent_environment("openclaw")

        mock_binary.assert_called_once_with("openclaw")

    def test_propagates_error_from_check(self, mocker: MockerFixture) -> None:
        """Test that errors from check are propagated."""

        def failing_check() -> None:
            raise AgentEnvironmentError("Check failed")

        mocker.patch("swe_runner.agents.env_checks.check_docker_available")
        mocker.patch("swe_runner.agents.env_checks.check_disk_space")
        mocker.patch("swe_runner.agents.env_checks.check_agent_binary")

        register_agent_descriptor(AgentDescriptor(name="failing-agent-test", environment_checks=(failing_check,)))
        try:
            with pytest.raises(AgentEnvironmentError, match="Check failed"):
                check_agent_environment("failing-agent-test")
        finally:
            AGENT_DESCRIPTOR_REGISTRY.pop("failing-agent-test", None)
