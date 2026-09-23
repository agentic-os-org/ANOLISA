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

"""OpenClaw-safe identifiers for local agents and sessions."""

from __future__ import annotations

import uuid


def safe_session_component(value: str) -> str:
    """Return a filesystem-friendly session path component."""
    chars = [char if char.isalnum() or char in "._-" else "-" for char in value]
    safe = "".join(chars).strip("-._")
    return safe or "instance"


def _safe_agent_id_component(value: str) -> str:
    chars = [char.lower() if char.isalnum() else char if char in "_-" else "-" for char in value]
    safe = "".join(chars).strip("-_")
    return safe or "agent"


def build_openclaw_agent_id(instance_id: str) -> str:
    """Use the SWE-bench instance id as the OpenClaw local agent id."""
    return _safe_agent_id_component(instance_id)[:63].strip("-_") or "case"


def build_openclaw_session_id(instance_id: str) -> str:
    """Create a stable-looking explicit session id for the OpenClaw local CLI."""
    return f"{safe_session_component(instance_id)}-{uuid.uuid4().hex[:8]}"
