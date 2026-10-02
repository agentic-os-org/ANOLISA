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

"""OpenClaw input manifest collector registration."""

from __future__ import annotations

from collections.abc import Mapping

from swe_runner.agents.input_manifest import CollectedInputManifest
from swe_runner.agents.lifecycle import PreparedAgentRun
from swe_runner.agents.openclaw.artifacts import OpenClawArtifacts
from swe_runner.run.io.manifest_records import directory_tree_record, file_record, redacted_json_file


def collect_openclaw_input_manifest(
    prepared: PreparedAgentRun,
    metadata: Mapping[str, object],
) -> CollectedInputManifest:
    """Collect OpenClaw-specific files and sections for the input manifest."""
    artifacts = OpenClawArtifacts.from_metadata(metadata)
    has_openclaw_metadata = any(
        (
            artifacts.openclaw_profile,
            artifacts.openclaw_profile_dir,
            artifacts.openclaw_config_path,
            artifacts.openclaw_workspace_root,
            artifacts.openclaw_injection_mode,
            artifacts.openclaw_agents_path,
        )
    )
    if prepared.settings.agent.name != "openclaw" and not has_openclaw_metadata:
        return CollectedInputManifest()

    openclaw_config_path = artifacts.openclaw_config_path
    openclaw_profile_dir = artifacts.openclaw_profile_dir
    openclaw_agents_path = artifacts.openclaw_agents_path

    return CollectedInputManifest(
        files={
            "openclaw_agents": file_record(openclaw_agents_path),
            "openclaw_config": file_record(openclaw_config_path),
            "openclaw_profile_dir": directory_tree_record(openclaw_profile_dir),
        },
        sections={
            "openclaw": {
                "profile": artifacts.openclaw_profile,
                "profile_dir": openclaw_profile_dir,
                "config_path": openclaw_config_path,
                "injection_mode": artifacts.openclaw_injection_mode,
                "config_redacted": redacted_json_file(openclaw_config_path),
            }
        },
    )
