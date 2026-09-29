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

"""OpenClaw-specific run artifact metadata."""

from __future__ import annotations

from collections.abc import Mapping

from pydantic import BaseModel, Field


class OpenClawArtifacts(BaseModel):
    """Metadata emitted by the OpenClaw adapter and consumed by OpenClaw-aware outputs."""

    base_agent_id: str | None = None
    openclaw_profile: str | None = None
    openclaw_profile_dir: str | None = None
    openclaw_config_path: str | None = None
    openclaw_workspace_root: str | None = None
    openclaw_injection_mode: str | None = None
    openclaw_agents_path: str | None = None
    openclaw_returncode: str | None = None
    openclaw_error_log: str | None = None
    openclaw_tokenless_requested: str | None = None
    openclaw_tokenless_evidence_path: str | None = None
    openclaw_tokenless_evidence_strong: str | None = None
    openclaw_tokenless_plugin_loaded: str | None = None
    openclaw_tokenless_hook_seen: str | None = None
    openclaw_tokenless_exec_tool_calls: str | None = None
    openclaw_tokenless_evidence_error: str | None = None
    extra_metadata: dict[str, str] = Field(default_factory=dict)

    @classmethod
    def from_metadata(cls, metadata: Mapping[str, object] | None) -> OpenClawArtifacts:
        """Build OpenClaw artifacts from persisted metadata."""
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
        """Return the persisted metadata representation."""
        metadata = dict(self.extra_metadata)
        for key, value in self.model_dump(exclude={"extra_metadata"}).items():
            if isinstance(value, str) and value:
                metadata[key] = value
        return metadata

    def with_updates(self, **updates: str | None) -> OpenClawArtifacts:
        """Return a copy with OpenClaw metadata fields updated."""
        return self.model_copy(update={key: value for key, value in updates.items() if value is not None})
