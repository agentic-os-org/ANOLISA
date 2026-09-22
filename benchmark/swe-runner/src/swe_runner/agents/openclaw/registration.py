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

"""OpenClaw agent capability registration."""

from __future__ import annotations

from swe_runner.agents.openclaw.input_manifest import collect_openclaw_input_manifest
from swe_runner.agents.openclaw.metadata import collect_openclaw_metadata
from swe_runner.agents.registry import AgentDescriptor, register_agent_descriptor

register_agent_descriptor(
    AgentDescriptor(
        name="openclaw",
        adapter_module="swe_runner.agents.openclaw.adapter",
        required_binaries=("openclaw",),
        supported_run_options=("tokenless",),
        metadata_collectors=(collect_openclaw_metadata,),
        input_manifest_collectors=(collect_openclaw_input_manifest,),
    )
)
