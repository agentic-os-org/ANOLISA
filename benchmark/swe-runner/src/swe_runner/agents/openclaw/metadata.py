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

"""OpenClaw run metadata collector registration."""

from __future__ import annotations

from collections.abc import Mapping

from swe_runner.agents.metadata import CollectedMetadata
from swe_runner.agents.openclaw.artifacts import OpenClawArtifacts


def collect_openclaw_metadata(metadata: Mapping[str, object]) -> CollectedMetadata:
    """Collect OpenClaw-specific metadata fields for runner outputs."""
    artifacts = OpenClawArtifacts.from_metadata(metadata)
    instance_result_fields: dict[str, str] = {}
    run_metadata_mappings: dict[str, str] = {}

    if artifacts.openclaw_returncode:
        instance_result_fields["openclaw_returncode"] = artifacts.openclaw_returncode
    if artifacts.openclaw_error_log:
        instance_result_fields["openclaw_error_log"] = artifacts.openclaw_error_log
    if artifacts.openclaw_profile_dir:
        run_metadata_mappings["openclaw_profile_dirs"] = artifacts.openclaw_profile_dir

    return CollectedMetadata(
        instance_result_fields=instance_result_fields,
        run_metadata_mappings=run_metadata_mappings,
    )
