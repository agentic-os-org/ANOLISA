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

import tempfile
from pathlib import Path
from unittest.mock import MagicMock, patch

from swe_runner.agents.cosh.adapter import CoshAdapter
from swe_runner.agents.lifecycle import PreparedAgentRun
from swe_runner.common.models import AgentConfig, AgentResult, Settings, SWEInstance
from swe_runner.run.execution.orchestrator import Orchestrator
from swe_runner.run.workspace.docker_refs import get_docker_image_name


class FakeFailAdapter(CoshAdapter):
    @property
    def name(self) -> str:
        return "fake-fail"

    def run(self, prepared: PreparedAgentRun) -> AgentResult:
        raise RuntimeError("Agent crashed")


def _make_mock_docker():
    mock = MagicMock()
    mock.start.return_value = Path("/tmp/fake_work")
    mock.container_id = "fake-id-123"
    mock.container_name = "swe-runner-fake"
    mock.cleanup.return_value = None
    return mock


def test_e2e_failure_with_fake_agent(sample_instance):
    with tempfile.TemporaryDirectory() as tmpdir:
        output_dir = Path(tmpdir)
        agent = FakeFailAdapter()
        settings = Settings(agent=AgentConfig(name="fake-fail"))

        with patch("swe_runner.agents.cosh.adapter.DockerManager", return_value=_make_mock_docker()):
            orchestrator = Orchestrator(agent, settings)
            result = orchestrator.run_single(sample_instance, output_dir)

        assert result.success is False
        assert result.prediction is None


def test_docker_image_name_format():
    instance = SWEInstance(
        instance_id="django__django-12345",
        repo="django/django",
        version="3.0",
        base_commit="abc",
        problem_statement="test",
        patch="",
        test_patch="",
    )
    name = get_docker_image_name(instance)
    assert name == "swebench/sweb.eval.x86_64.django_1776_django-12345:latest"
