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

"""Retry unsuccessful saved runs while preserving successful benchmark work."""

import json
from contextlib import nullcontext
from pathlib import Path
from unittest.mock import Mock, patch

import pytest
from swe_runner.cli import app
from swe_runner.common.models import AgentResult, InstanceResult, OutputConfig, Settings, SWEInstance
from swe_runner.run.execution.orchestrator import Orchestrator
from swe_runner.run.io.output_store import RunOutputStore
from swe_runner.run.io.report import RunReport
from swe_runner.run.session import RunSession
from typer.testing import CliRunner


def test_load_successful_result_ids_requires_explicit_true(tmp_path: Path) -> None:
    results_dir = tmp_path / "results"
    results_dir.mkdir()
    for instance_id, success in [("ok", True), ("failed", False), ("string", "true"), ("number", 1), ("missing", None)]:
        (results_dir / f"{instance_id}.json").write_text(json.dumps({"instance_id": instance_id, "success": success}))
    (results_dir / "invalid.json").write_text("{broken")

    store = RunOutputStore(tmp_path)

    assert store.load_attempted_instance_ids(successful_only=True) == {"ok"}
    assert store.load_attempted_instance_ids() == {"ok", "failed", "string", "number", "missing"}


def test_retry_failed_runs_failed_and_new_instances(tmp_path: Path, sample_instance: SWEInstance) -> None:
    results_dir = tmp_path / "results"
    results_dir.mkdir()
    (results_dir / "ok.json").write_text('{"instance_id":"ok","success":true}')
    (results_dir / "failed.json").write_text('{"instance_id":"failed","success":false}')
    (results_dir / "unknown.json").write_text('{"instance_id":"unknown"}')
    instances = [
        sample_instance.model_copy(update={"instance_id": value}) for value in ["ok", "failed", "unknown", "new"]
    ]
    agent = Mock()
    agent.name = "fake"
    orchestrator = Orchestrator(agent, retry_failed=True, progress_factory=lambda total: nullcontext(Mock()))
    result = InstanceResult(
        instance=sample_instance,
        prediction=None,
        agent_result=AgentResult(raw_output="", patch=None, success=False, duration_seconds=0),
        success=False,
    )

    with patch.object(orchestrator, "run_single", return_value=result) as run_single:
        results = orchestrator.run_batch(instances, tmp_path)

    assert len(results) == 3
    assert {call.args[0].instance_id for call in run_single.call_args_list} == {"failed", "unknown", "new"}
    assert [item.instance_id for item in agent.prepare_batch.call_args.args[0]] == ["failed", "unknown", "new"]
    agent.post_batch.assert_called_once_with()
    assert json.loads((results_dir / "ok.json").read_text())["success"] is True


def test_retry_failed_conflicts_with_redo() -> None:
    with pytest.raises(ValueError, match="mutually exclusive"):
        Orchestrator(Mock(), redo=True, retry_failed=True)
    with pytest.raises(ValueError, match="mutually exclusive"):
        RunSession(Settings(), redo=True, retry_failed=True)


def test_cli_retry_failed_reaches_session(tmp_path: Path) -> None:
    with patch("swe_runner.cli_commands.RunSession") as session:
        session.return_value.execute.return_value = RunReport(succeeded=0, failed=0, total=0, instance_ids=[])
        result = CliRunner().invoke(app, ["run", "--agent", "cosh", "--retry-failed", "--output", str(tmp_path)])

    assert result.exit_code == 0
    assert session.call_args.kwargs["retry_failed"] is True


def test_cli_rejects_conflicting_resume_modes_before_session(tmp_path: Path) -> None:
    with patch("swe_runner.cli_commands.RunSession") as session:
        result = CliRunner().invoke(
            app, ["run", "--agent", "cosh", "--redo", "--retry-failed", "--output", str(tmp_path)]
        )

    assert result.exit_code == 1
    assert "mutually exclusive" in result.output
    session.assert_not_called()


def test_session_forwards_retry_failed_to_orchestrator(tmp_path: Path, sample_instance: SWEInstance) -> None:
    settings = Settings(output=OutputConfig(output_dir=tmp_path))
    session = RunSession(settings, retry_failed=True)
    with (
        patch("swe_runner.run.session.check_agent_environment"),
        patch("swe_runner.run.session.get_agent", return_value=Mock()),
        patch("swe_runner.run.session.load_dataset", return_value=[sample_instance]),
        patch("swe_runner.run.session.Orchestrator") as orchestrator,
    ):
        orchestrator.return_value.run_batch.return_value = []
        session.execute()

    assert orchestrator.call_args.kwargs["retry_failed"] is True
