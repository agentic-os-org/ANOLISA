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

"""Single-instance execution finalization."""

from __future__ import annotations

from pathlib import Path

from swe_runner.agents import PreparedAgentRun
from swe_runner.common.models import AgentResult, InstanceResult
from swe_runner.run.execution.finalization import finalize_instance_result
from swe_runner.run.io.output_store import RunOutputStore


def complete_instance_run(
    *,
    agent_name: str,
    prepared: PreparedAgentRun,
    agent_result: AgentResult,
    output_dir: Path,
) -> InstanceResult:
    """Extract the final patch, save outputs, and clean up prepared resources."""
    result = finalize_instance_result(agent_name=agent_name, prepared=prepared, agent_result=agent_result)
    RunOutputStore(output_dir).save_instance_result(result)
    return result
