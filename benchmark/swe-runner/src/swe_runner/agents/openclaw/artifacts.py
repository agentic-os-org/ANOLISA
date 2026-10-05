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

from swe_runner.common.artifact_metadata import ArtifactMetadata


class OpenClawArtifacts(ArtifactMetadata):
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
