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

"""Duplicate cases cannot enter shared per-instance execution paths."""

from contextlib import nullcontext
from pathlib import Path
from unittest.mock import Mock, patch

import pytest
from swe_runner.cli import app
from swe_runner.common.models import AgentResult, InstanceResult, SWEInstance
from swe_runner.run.execution.orchestrator import DuplicateInstanceError, Orchestrator
from typer.testing import CliRunner


@pytest.mark.parametrize("redo", [False, True])
def test_duplicate_ids_are_rejected_before_agent_preparation(
    tmp_path: Path, sample_instance: SWEInstance, redo: bool
) -> None:
    instances = [
        sample_instance.model_copy(update={"instance_id": instance_id, "repo": f"repo/{index}"})
        for index, instance_id in enumerate(["z-case", "a-case", "z-case", "a-case"])
    ]
    agent = Mock()
    agent.name = "fake"
    orchestrator = Orchestrator(agent, redo=redo, progress_factory=lambda total: nullcontext(Mock()))
    result = InstanceResult(
        instance=sample_instance,
        prediction=None,
        agent_result=AgentResult(raw_output="", patch=None, success=False, duration_seconds=0),
        success=False,
    )

    with (
        patch.object(orchestrator, "run_single", return_value=result) as run_single,
        pytest.raises(DuplicateInstanceError, match="Duplicate instance IDs.*a-case, z-case"),
    ):
        orchestrator.run_batch(instances, tmp_path)

    run_single.assert_not_called()
    agent.prepare_batch.assert_not_called()
    agent.post_batch.assert_not_called()


def test_duplicate_batch_cli_error_reports_instance_ids(tmp_path: Path) -> None:
    with patch(
        "swe_runner.cli_commands.RunSession.execute",
        side_effect=DuplicateInstanceError("Duplicate instance IDs in selected batch: case-1"),
    ):
        result = CliRunner().invoke(app, ["run", "--agent", "cosh", "--output", str(tmp_path)])

    assert result.exit_code == 1
    assert "Error:" in result.output
    assert "Duplicate instance IDs" in result.output
    assert "case-1" in result.output
