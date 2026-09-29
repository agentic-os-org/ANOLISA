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

"""Tests for single-instance lifecycle orchestration."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import Mock, patch

from swe_runner.agents import AgentAdapter, PreparedAgentRun
from swe_runner.common.models import AgentResult, InstanceResult, Settings, SWEInstance
from swe_runner.run.execution.instance_lifecycle import InstanceRunLifecycle


def _instance(instance_id: str = "inst-1") -> SWEInstance:
    return SWEInstance(
        instance_id=instance_id,
        repo="example/repo",
        version="1.0",
        base_commit="abc123",
        problem_statement="Fix it",
        patch="",
        test_patch="",
    )


def _prepared(instance: SWEInstance, *, cleanup: Mock | None = None) -> PreparedAgentRun:
    return PreparedAgentRun(
        instance=instance,
        settings=Settings(),
        work_dir=Path("/tmp/work"),
        prompt="Fix it",
        timeout=1200,
        max_turns=0,
        metadata={"session_id": "sess-1"},
        cleanup_callbacks=[cleanup] if cleanup is not None else [],
    )


class FakeAdapter(AgentAdapter):
    def __init__(
        self,
        *,
        prepared: PreparedAgentRun | None = None,
        prepare_error: Exception | None = None,
        run_error: Exception | None = None,
    ) -> None:
        self.prepared = prepared
        self.prepare_error = prepare_error
        self.run_error = run_error
        self.run_calls = 0

    @property
    def name(self) -> str:
        return "fake"

    def prepare(self, instance: SWEInstance, settings: Settings) -> PreparedAgentRun:
        del settings
        if self.prepare_error is not None:
            raise self.prepare_error
        if self.prepared is not None:
            return self.prepared
        return _prepared(instance)

    def run(self, prepared: PreparedAgentRun) -> AgentResult:
        del prepared
        self.run_calls += 1
        if self.run_error is not None:
            raise self.run_error
        return AgentResult(raw_output="ok", patch=None, success=True, duration_seconds=1.0)


def test_prepare_failure_is_saved_without_running_agent(tmp_path: Path) -> None:
    adapter = FakeAdapter(prepare_error=RuntimeError("docker unavailable"))

    result = InstanceRunLifecycle(adapter, Settings(), tmp_path).run(_instance())

    assert result.success is False
    assert result.agent_result.error == "Prepare error: docker unavailable"
    assert adapter.run_calls == 0
    assert (tmp_path / "results" / "inst-1.json").exists()


def test_manifest_failure_cleans_prepared_run_and_saves_failure(tmp_path: Path) -> None:
    instance = _instance()
    cleanup = Mock()
    adapter = FakeAdapter(prepared=_prepared(instance, cleanup=cleanup))

    with patch(
        "swe_runner.run.execution.instance_lifecycle.write_input_manifest", side_effect=RuntimeError("manifest boom")
    ):
        result = InstanceRunLifecycle(adapter, Settings(), tmp_path).run(instance)

    assert result.success is False
    assert result.agent_result.error == "Input manifest error: manifest boom"
    assert adapter.run_calls == 0
    cleanup.assert_called_once_with()
    assert (tmp_path / "results" / "inst-1.json").exists()


def test_agent_failure_is_passed_to_finalizer_with_prepared_metadata(tmp_path: Path) -> None:
    instance = _instance()
    prepared = _prepared(instance)
    adapter = FakeAdapter(prepared=prepared, run_error=RuntimeError("agent boom"))
    finalized_result = InstanceResult(
        instance=instance,
        prediction=None,
        agent_result=AgentResult(raw_output="agent boom", patch=None, success=False, duration_seconds=0.0),
        success=False,
    )

    with (
        patch(
            "swe_runner.run.execution.instance_lifecycle.write_input_manifest", return_value=tmp_path / "manifest.json"
        ),
        patch(
            "swe_runner.run.execution.instance_lifecycle.complete_instance_run", return_value=finalized_result
        ) as mock_complete,
    ):
        result = InstanceRunLifecycle(adapter, Settings(), tmp_path).run(instance)

    assert result is finalized_result
    agent_result = mock_complete.call_args.kwargs["agent_result"]
    assert agent_result.success is False
    assert agent_result.error == "agent boom"
    assert agent_result.metadata == {
        "session_id": "sess-1",
        "input_manifest_path": str(tmp_path / "manifest.json"),
    }
