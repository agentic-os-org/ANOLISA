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

"""Typed run artifact metadata shared by runner outputs and agents."""

from __future__ import annotations

from collections.abc import Mapping

from swe_runner.common.artifact_metadata import ArtifactMetadata


class RunArtifacts(ArtifactMetadata):
    """Known metadata emitted while running one SWE-bench instance."""

    docker_image_name: str | None = None
    input_manifest_path: str | None = None
    agent_id: str | None = None
    session_id: str | None = None


def merge_metadata(*metadata_items: Mapping[str, object] | None) -> dict[str, str]:
    """Merge metadata in order while preserving typed artifact semantics."""
    merged: dict[str, str] = {}
    for metadata in metadata_items:
        merged.update(RunArtifacts.from_metadata(metadata).to_metadata())
    return merged
