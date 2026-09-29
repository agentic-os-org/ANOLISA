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

from pydantic import BaseModel, Field


class RunArtifacts(BaseModel):
    """Known metadata emitted while running one SWE-bench instance."""

    docker_image_name: str | None = None
    input_manifest_path: str | None = None
    agent_id: str | None = None
    session_id: str | None = None
    extra_metadata: dict[str, str] = Field(default_factory=dict)

    @classmethod
    def from_metadata(cls, metadata: Mapping[str, object] | None) -> RunArtifacts:
        """Build typed artifacts from persisted metadata while preserving unknown keys."""
        if metadata is None:
            return cls()

        known_fields = set(cls.model_fields) - {"extra_metadata"}
        known_values: dict[str, str] = {}
        extra_metadata: dict[str, str] = {}
        for key, value in metadata.items():
            if not isinstance(key, str) or not isinstance(value, str):
                continue
            if key in known_fields:
                known_values[key] = value
            else:
                extra_metadata[key] = value
        return cls(**known_values, extra_metadata=extra_metadata)

    def to_metadata(self) -> dict[str, str]:
        """Return the JSON-compatible metadata representation."""
        metadata = dict(self.extra_metadata)
        for key, value in self.model_dump(exclude={"extra_metadata"}).items():
            if isinstance(value, str) and value:
                metadata[key] = value
        return metadata

    def with_updates(self, **updates: str | None) -> RunArtifacts:
        """Return a copy with known metadata fields updated."""
        return self.model_copy(update={key: value for key, value in updates.items() if value is not None})


def merge_metadata(*metadata_items: Mapping[str, object] | None) -> dict[str, str]:
    """Merge metadata in order while preserving typed artifact semantics."""
    merged: dict[str, str] = {}
    for metadata in metadata_items:
        merged.update(RunArtifacts.from_metadata(metadata).to_metadata())
    return merged
