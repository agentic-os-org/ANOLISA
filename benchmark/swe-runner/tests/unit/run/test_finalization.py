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

"""Tests for instance finalization."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import Mock, patch

from swe_runner.agents import PreparedAgentRun
from swe_runner.common.models import AgentResult, Settings, SWEInstance
from swe_runner.run.execution.finalization import finalize_instance_result

SAMPLE_DIFF = "diff --git a/foo.py b/foo.py\n@@ -1 +1 @@\n-old\n+new\n"


def _instance() -> SWEInstance:
    return SWEInstance(
        instance_id="inst-1",
        repo="example/repo",
        version="1.0",
        base_commit="abc123",
        problem_statement="Fix it",
        patch="",
        test_patch="",
    )


def _prepared(cleanup: Mock) -> PreparedAgentRun:
    return PreparedAgentRun(
        instance=_instance(),
        settings=Settings(),
        work_dir=Path("/tmp/work"),
        prompt="Fix it",
        timeout=1200,
        max_turns=0,
        base_revision="base-rev",
        metadata={"session_id": "sess-1"},
        cleanup_callbacks=[cleanup],
    )


def test_finalize_success_builds_prediction_and_cleans_up() -> None:
    cleanup = Mock()
    prepared = _prepared(cleanup)
    agent_result = AgentResult(raw_output="ok", patch=None, success=True, duration_seconds=1.0)

    with patch("swe_runner.run.execution.finalization.patches.extract_patch", return_value=SAMPLE_DIFF) as mock_extract:
        result = finalize_instance_result(agent_name="cosh", prepared=prepared, agent_result=agent_result)

    assert result.success is True
    assert result.prediction is not None
    assert result.prediction.model_name_or_path == "cosh"
    assert result.prediction.model_patch == SAMPLE_DIFF
    assert result.agent_result.patch == SAMPLE_DIFF
    mock_extract.assert_called_once_with(Path("/tmp/work"), instance_id="inst-1", base_revision="base-rev")
    cleanup.assert_called_once_with()


def test_finalize_no_patch_is_unsuccessful_without_prediction() -> None:
    cleanup = Mock()
    agent_result = AgentResult(raw_output="ok", patch=None, success=True, duration_seconds=1.0)

    with patch("swe_runner.run.execution.finalization.patches.extract_patch", return_value=None):
        result = finalize_instance_result(agent_name="cosh", prepared=_prepared(cleanup), agent_result=agent_result)

    assert result.success is False
    assert result.prediction is None
    assert result.agent_result.patch is None
    cleanup.assert_called_once_with()


def test_finalize_failed_agent_keeps_patch_but_not_prediction() -> None:
    cleanup = Mock()
    agent_result = AgentResult(
        raw_output="agent failed",
        patch=None,
        success=False,
        duration_seconds=1.0,
        error="agent failed",
    )

    with patch("swe_runner.run.execution.finalization.patches.extract_patch", return_value=SAMPLE_DIFF):
        result = finalize_instance_result(agent_name="cosh", prepared=_prepared(cleanup), agent_result=agent_result)

    assert result.success is False
    assert result.prediction is None
    assert result.agent_result.patch == SAMPLE_DIFF
    assert result.agent_result.error == "agent failed"
    cleanup.assert_called_once_with()


def test_finalize_patch_error_returns_post_processing_failure_and_cleans_up() -> None:
    cleanup = Mock()
    agent_result = AgentResult(
        raw_output="agent output",
        patch=None,
        success=True,
        duration_seconds=1.5,
        metadata={"session_id": "sess-1"},
    )

    with patch("swe_runner.run.execution.finalization.patches.extract_patch", side_effect=RuntimeError("git failed")):
        result = finalize_instance_result(agent_name="cosh", prepared=_prepared(cleanup), agent_result=agent_result)

    assert result.success is False
    assert result.prediction is None
    assert result.agent_result.raw_output == "agent output"
    assert result.agent_result.duration_seconds == 1.5
    assert result.agent_result.error == "Post-processing error: git failed"
    assert result.agent_result.metadata == {"session_id": "sess-1"}
    cleanup.assert_called_once_with()
