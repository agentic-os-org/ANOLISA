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

"""Shared persistence contract for typed artifact metadata."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Self

from pydantic import BaseModel, Field


class ArtifactMetadata(BaseModel):
    """String metadata with model-specific known fields and preserved overflow keys."""

    extra_metadata: dict[str, str] = Field(default_factory=dict)

    @classmethod
    def from_metadata(cls, metadata: Mapping[str, object] | None) -> Self:
        """Build the concrete artifact model while preserving unknown string values."""
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
        """Return persisted string values with nonempty known fields taking precedence."""
        metadata = dict(self.extra_metadata)
        for key, value in self.model_dump(exclude={"extra_metadata"}).items():
            if isinstance(value, str) and value:
                metadata[key] = value
        return metadata

    def with_updates(self, **updates: str | None) -> Self:
        """Return a concrete-model copy while keeping existing values for None updates."""
        return self.model_copy(update={key: value for key, value in updates.items() if value is not None})
