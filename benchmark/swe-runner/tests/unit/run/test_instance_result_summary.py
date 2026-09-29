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

from swe_runner.common.models import AgentResult, InstanceResult, Prediction, SWEInstance
from swe_runner.run.io.instance_result_summary import build_instance_result_summary


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


def test_build_instance_result_summary_includes_common_and_agent_metadata_fields() -> None:
    result = InstanceResult(
        instance=_instance(),
        prediction=Prediction(instance_id="inst-1", model_name_or_path="cosh", model_patch="diff"),
        agent_result=AgentResult(
            raw_output="ok",
            patch="diff",
            success=True,
            duration_seconds=1.5,
            metadata={
                "session_id": "sess-1",
                "openclaw_returncode": "0",
            },
        ),
        success=True,
    )

    assert build_instance_result_summary(result) == {
        "instance_id": "inst-1",
        "success": True,
        "error": None,
        "duration_seconds": 1.5,
        "patch_produced": True,
        "agent_name": "cosh",
        "metadata": {
            "session_id": "sess-1",
            "openclaw_returncode": "0",
        },
        "session_id": "sess-1",
        "openclaw_returncode": "0",
    }


def test_build_instance_result_summary_handles_failures_without_prediction() -> None:
    result = InstanceResult(
        instance=_instance("inst-2"),
        prediction=None,
        agent_result=AgentResult(
            raw_output="failed",
            patch=None,
            success=False,
            duration_seconds=2.0,
            error="Timeout",
        ),
        success=False,
    )

    summary = build_instance_result_summary(result)

    assert summary["instance_id"] == "inst-2"
    assert summary["success"] is False
    assert summary["patch_produced"] is False
    assert summary["agent_name"] is None
    assert summary["error"] == "Timeout"
