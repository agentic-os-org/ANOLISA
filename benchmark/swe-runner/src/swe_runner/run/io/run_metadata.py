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

"""Run metadata payload construction and merging."""

from __future__ import annotations

import datetime as dt
from dataclasses import dataclass, field
from typing import Any

RUN_METADATA_CORE_KEYS: frozenset[str] = frozenset(
    {
        "started_at_ns",
        "ended_at_ns",
        "started_at",
        "ended_at",
        "agent_name",
        "agent_names",
        "workers",
        "instance_ids",
        "instance_count",
        "attempt_count",
        "succeeded",
        "failed",
        "run_count",
    }
)


@dataclass(frozen=True)
class RunMetadataSnapshot:
    """In-memory representation of one run's metadata contribution."""

    started_at_ns: int
    ended_at_ns: int
    agent_name: str
    workers: int
    instance_ids: list[str]
    succeeded: int
    metadata_mappings: dict[str, dict[str, str]] = field(default_factory=dict)

    def to_payload(self) -> dict[str, Any]:
        """Convert the snapshot into the persisted run metadata shape."""
        payload: dict[str, Any] = {
            "started_at_ns": self.started_at_ns,
            "ended_at_ns": self.ended_at_ns,
            "started_at": ns_to_iso(self.started_at_ns),
            "ended_at": ns_to_iso(self.ended_at_ns),
            "agent_name": self.agent_name,
            "workers": self.workers,
            "instance_ids": self.instance_ids,
            "instance_count": len(self.instance_ids),
            "attempt_count": len(self.instance_ids),
            "succeeded": self.succeeded,
            "failed": len(self.instance_ids) - self.succeeded,
            "run_count": 1,
        }
        for key, values in self.metadata_mappings.items():
            if values:
                payload[key] = values
        return payload


def ns_to_iso(ns: int) -> str:
    """Convert a nanosecond timestamp to UTC ISO-8601."""
    return dt.datetime.fromtimestamp(ns / 1e9, tz=dt.UTC).isoformat()


def merge_run_metadata(existing: dict[str, Any] | None, current: dict[str, Any]) -> dict[str, Any]:
    """Merge a current run metadata payload into an existing payload."""
    if existing is None:
        return current

    started_at_ns = min(_safe_int(existing.get("started_at_ns")), _safe_int(current.get("started_at_ns")))
    ended_at_ns = max(_safe_int(existing.get("ended_at_ns")), _safe_int(current.get("ended_at_ns")))
    instance_ids = _ordered_unique_strings(
        [
            item
            for value in (existing.get("instance_ids"), current.get("instance_ids"))
            if isinstance(value, list)
            for item in value
            if isinstance(item, str) and item
        ]
    )
    existing_agent_names = existing.get("agent_names")
    agent_names = _ordered_unique_strings(
        [
            item
            for item in [
                *(_metadata_string_values(existing_agent_names) if isinstance(existing_agent_names, list) else []),
                existing.get("agent_name"),
                current.get("agent_name"),
            ]
            if isinstance(item, str) and item
        ]
    )

    merged: dict[str, Any] = {
        "started_at_ns": started_at_ns,
        "ended_at_ns": ended_at_ns,
        "started_at": ns_to_iso(started_at_ns),
        "ended_at": ns_to_iso(ended_at_ns),
        "agent_name": current.get("agent_name"),
        "workers": current.get("workers"),
        "instance_ids": instance_ids,
        "instance_count": len(instance_ids),
        "attempt_count": _safe_int(existing.get("attempt_count") or existing.get("instance_count"))
        + _safe_int(current.get("attempt_count") or current.get("instance_count")),
        "succeeded": _safe_int(existing.get("succeeded")) + _safe_int(current.get("succeeded")),
        "failed": _safe_int(existing.get("failed")) + _safe_int(current.get("failed")),
        "run_count": _safe_int(existing.get("run_count")) + _safe_int(current.get("run_count")),
    }
    if agent_names:
        merged["agent_names"] = agent_names

    for key in sorted(_run_metadata_mapping_keys(existing, current)):
        metadata_mapping = _merge_metadata_mapping(existing.get(key), current.get(key))
        if metadata_mapping:
            merged[key] = metadata_mapping
    return merged


def _ordered_unique_strings(values: list[str]) -> list[str]:
    seen: set[str] = set()
    ordered: list[str] = []
    for value in values:
        if value in seen:
            continue
        seen.add(value)
        ordered.append(value)
    return ordered


def _metadata_string_values(value: Any) -> list[str]:
    if isinstance(value, dict):
        return [item for item in value.values() if isinstance(item, str) and item]
    if isinstance(value, list):
        return [item for item in value if isinstance(item, str) and item]
    return []


def _merge_metadata_mapping(
    old_value: Any,
    new_value: dict[str, str] | None,
) -> dict[str, str] | None:
    """Merge instance keyed metadata; later runs overwrite the same instance key."""
    merged: dict[str, str] = {}

    if isinstance(old_value, dict):
        for raw_key, raw_item in old_value.items():
            if isinstance(raw_key, str) and isinstance(raw_item, str) and raw_item:
                merged[raw_key] = raw_item

    if new_value:
        merged.update(new_value)

    return merged or None


def _is_metadata_mapping(value: Any) -> bool:
    return isinstance(value, dict) and all(
        isinstance(raw_key, str) and isinstance(raw_item, str) and bool(raw_item) for raw_key, raw_item in value.items()
    )


def _run_metadata_mapping_keys(*payloads: dict[str, Any]) -> set[str]:
    keys: set[str] = set()
    for payload in payloads:
        for key, value in payload.items():
            if key not in RUN_METADATA_CORE_KEYS and _is_metadata_mapping(value):
                keys.add(key)
    return keys


def _safe_int(value: Any) -> int:
    return value if isinstance(value, int) else 0
