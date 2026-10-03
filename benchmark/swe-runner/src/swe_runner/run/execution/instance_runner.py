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

"""Public single-instance execution entry point."""

from __future__ import annotations

from pathlib import Path

from swe_runner.agents import AgentAdapter
from swe_runner.common.models import InstanceResult, Settings, SWEInstance
from swe_runner.run.execution.instance_lifecycle import InstanceRunLifecycle


def run_instance(agent: AgentAdapter, settings: Settings, instance: SWEInstance, output_dir: Path) -> InstanceResult:
    """Run one instance through the agent prepare/run/post lifecycle."""
    return InstanceRunLifecycle(agent, settings, output_dir).run(instance)
