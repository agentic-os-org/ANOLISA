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

"""Agent-specific input manifest collector registry."""

from __future__ import annotations

from collections.abc import Callable, Mapping
from dataclasses import dataclass, field
from typing import Any, cast

from swe_runner.agents.lifecycle import PreparedAgentRun
from swe_runner.agents.registry import iter_input_manifest_collectors


@dataclass(frozen=True)
class CollectedInputManifest:
    """Input manifest fields contributed by an agent collector."""

    files: dict[str, Any] = field(default_factory=dict)
    sections: dict[str, Any] = field(default_factory=dict)


InputManifestCollector = Callable[[PreparedAgentRun, Mapping[str, object]], CollectedInputManifest]

_LEGACY_REGISTERED_COLLECTORS: list[InputManifestCollector] = []


def register_input_manifest_collector(collector: InputManifestCollector) -> None:
    """Register an agent-specific input manifest collector.

    Prefer declaring collectors through ``AgentDescriptor``. This function
    remains for third-party code using the old extension point.
    """
    if collector not in _LEGACY_REGISTERED_COLLECTORS:
        _LEGACY_REGISTERED_COLLECTORS.append(collector)


def collect_input_manifest(prepared: PreparedAgentRun, metadata: Mapping[str, object]) -> CollectedInputManifest:
    """Collect agent-specific input manifest files and sections."""
    collected_items = [
        cast(InputManifestCollector, collector)(prepared, metadata)
        for collector in iter_input_manifest_collectors()
    ]
    collected_items.extend(collector(prepared, metadata) for collector in _LEGACY_REGISTERED_COLLECTORS)
    return _merge_collected_input_manifests(collected_items)


def _merge_collected_input_manifests(items: list[CollectedInputManifest]) -> CollectedInputManifest:
    files: dict[str, Any] = {}
    sections: dict[str, Any] = {}
    for item in items:
        files.update(item.files)
        sections.update(item.sections)
    return CollectedInputManifest(files=files, sections=sections)
