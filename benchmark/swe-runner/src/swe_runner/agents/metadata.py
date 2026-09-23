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

"""Agent run metadata collection registry."""

from __future__ import annotations

from collections.abc import Callable, Mapping
from dataclasses import dataclass, field
from typing import cast

from swe_runner.agents.registry import iter_metadata_collectors


@dataclass(frozen=True)
class CollectedMetadata:
    """Metadata fields contributed by common runner artifacts and agent collectors."""

    instance_result_fields: dict[str, str] = field(default_factory=dict)
    run_metadata_mappings: dict[str, str] = field(default_factory=dict)


MetadataCollector = Callable[[Mapping[str, object]], CollectedMetadata]

_LEGACY_REGISTERED_COLLECTORS: list[MetadataCollector] = []


def register_metadata_collector(collector: MetadataCollector) -> None:
    """Register an agent-specific metadata collector.

    Prefer declaring collectors through ``AgentDescriptor``. This function
    remains for third-party code using the old extension point.
    """
    if collector not in _LEGACY_REGISTERED_COLLECTORS:
        _LEGACY_REGISTERED_COLLECTORS.append(collector)


def collect_result_metadata(metadata: Mapping[str, object]) -> CollectedMetadata:
    """Collect common and agent-specific metadata from a result metadata mapping."""
    collected_items = [_collect_common_metadata(metadata)]
    collected_items.extend(cast(MetadataCollector, collector)(metadata) for collector in iter_metadata_collectors())
    collected_items.extend(collector(metadata) for collector in _LEGACY_REGISTERED_COLLECTORS)
    return _merge_collected_metadata(collected_items)


def _collect_common_metadata(metadata: Mapping[str, object]) -> CollectedMetadata:
    session_id = metadata.get("session_id")
    if not isinstance(session_id, str) or not session_id:
        return CollectedMetadata()
    return CollectedMetadata(
        instance_result_fields={"session_id": session_id},
        run_metadata_mappings={"session_ids": session_id},
    )


def _merge_collected_metadata(items: list[CollectedMetadata]) -> CollectedMetadata:
    instance_result_fields: dict[str, str] = {}
    run_metadata_mappings: dict[str, str] = {}
    for item in items:
        instance_result_fields.update(item.instance_result_fields)
        run_metadata_mappings.update(item.run_metadata_mappings)
    return CollectedMetadata(
        instance_result_fields=instance_result_fields,
        run_metadata_mappings=run_metadata_mappings,
    )
