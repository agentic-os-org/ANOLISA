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

"""Projection from instance results to persisted summary payloads."""

from __future__ import annotations

from typing import Any

from swe_runner.agents.metadata import collect_result_metadata
from swe_runner.common.models import InstanceResult


def build_instance_result_summary(result: InstanceResult) -> dict[str, Any]:
    """Return the JSON payload stored for one instance result."""
    summary: dict[str, Any] = {
        "instance_id": result.instance.instance_id,
        "success": result.success,
        "error": result.agent_result.error,
        "duration_seconds": result.agent_result.duration_seconds,
        "patch_produced": result.agent_result.patch is not None,
        "agent_name": result.prediction.model_name_or_path if result.prediction else None,
        "metadata": result.agent_result.metadata,
    }
    summary.update(collect_result_metadata(result.agent_result.metadata).instance_result_fields)
    return summary
