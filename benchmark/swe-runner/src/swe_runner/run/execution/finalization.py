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

"""Finalize prepared instance runs into persisted result models."""

from __future__ import annotations

import logging

import swe_runner.run.workspace.patches as patches
from swe_runner.agents import PreparedAgentRun
from swe_runner.common.models import AgentResult, InstanceResult, Prediction

logger = logging.getLogger(__name__)


def finalize_instance_result(
    *,
    agent_name: str,
    prepared: PreparedAgentRun,
    agent_result: AgentResult,
) -> InstanceResult:
    """Extract the final patch, build the prediction/result, and clean up prepared resources."""
    try:
        patch = patches.extract_patch(
            prepared.work_dir,
            instance_id=prepared.instance.instance_id,
            base_revision=prepared.base_revision,
        )
        agent_result = agent_result.model_copy(update={"patch": patch})
        success = agent_result.success and bool(patch)
        prediction = (
            Prediction(
                instance_id=prepared.instance.instance_id,
                model_name_or_path=agent_name,
                model_patch=patch,
            )
            if success
            else None
        )
        return InstanceResult(
            instance=prepared.instance,
            prediction=prediction,
            agent_result=agent_result,
            success=success,
        )
    except Exception as exc:
        logger.exception("INSTANCE_POST_FAILED instance=%s agent=%s", prepared.instance.instance_id, agent_name)
        return InstanceResult(
            instance=prepared.instance,
            prediction=None,
            agent_result=AgentResult(
                raw_output=agent_result.raw_output,
                patch=None,
                success=False,
                duration_seconds=agent_result.duration_seconds,
                error=f"Post-processing error: {exc}",
                metadata=agent_result.metadata,
            ),
            success=False,
        )
    finally:
        prepared.cleanup()
