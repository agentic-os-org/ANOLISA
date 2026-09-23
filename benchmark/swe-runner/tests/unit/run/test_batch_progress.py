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

from unittest.mock import MagicMock, patch

import pytest

from swe_runner.common.models import AgentResult, InstanceResult, SWEInstance
from swe_runner.run.execution.batch_progress import RichBatchProgress


def _result(instance_id: str, *, success: bool, duration: float) -> InstanceResult:
    instance = SWEInstance(
        instance_id=instance_id,
        repo="example/repo",
        version="1.0",
        base_commit="abc123",
        problem_statement="Fix",
        patch="",
        test_patch="",
    )
    return InstanceResult(
        instance=instance,
        prediction=None,
        agent_result=AgentResult(
            raw_output="",
            patch=None,
            success=success,
            duration_seconds=duration,
        ),
        success=success,
    )


def test_rich_batch_progress_updates_description_and_advances() -> None:
    progress = MagicMock()
    progress.__enter__.return_value = progress
    progress.add_task.return_value = 123

    with (
        patch("swe_runner.run.execution.batch_progress.Progress", return_value=progress),
        RichBatchProgress(total_instances=2) as reporter,
    ):
        reporter.record_completion(_result("inst-1", success=True, duration=1.25), workers=3)

    progress.add_task.assert_called_once_with("Processing instances", total=2)
    progress.update.assert_called_once()
    assert progress.update.call_args.args[0] == 123
    assert progress.update.call_args.kwargs["description"] == "[1/2] inst-1 \u2713 (1.2s)"
    progress.advance.assert_called_once_with(123)


def test_rich_batch_progress_must_be_started_before_recording() -> None:
    reporter = RichBatchProgress(total_instances=1)

    with pytest.raises(RuntimeError, match="not been started"):
        reporter.record_completion(_result("inst-1", success=False, duration=0.0), workers=1)
