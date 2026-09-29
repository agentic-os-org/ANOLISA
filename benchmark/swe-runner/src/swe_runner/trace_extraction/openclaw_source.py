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

"""OpenClaw JSONL trace source resolution and execution."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from swe_runner.trace_extraction.helpers import ExtractionError
from swe_runner.trace_extraction.recording import record_openclaw_jsonl_traces_in_window
from swe_runner.trace_extraction.sources import (
    TraceSourcePlan,
    TraceSourceResolveContext,
    register_trace_source_resolver,
)


@dataclass(frozen=True)
class OpenClawJsonlTraceSourcePlan:
    """Executable trace source plan for OpenClaw JSONL session files."""

    start_ns: int
    end_ns: int
    profiles_root: Path | None = None
    profile_dirs: list[Path] | None = None
    instance_ids: set[str] | None = None
    session_ids: set[str] | None = None
    name: str = "openclaw_jsonl"

    def collect(self, trace_root: Path) -> list[Path]:
        """Record OpenClaw JSONL traces into runner trace JSON files."""
        return record_openclaw_jsonl_traces_in_window(
            start_ns=self.start_ns,
            end_ns=self.end_ns,
            profiles_root=self.profiles_root.expanduser() if self.profiles_root is not None else None,
            profile_dirs=self.profile_dirs,
            trace_root=trace_root,
            instance_ids=self.instance_ids,
            session_ids=self.session_ids,
        )


def resolve_openclaw_jsonl_trace_source(context: TraceSourceResolveContext) -> TraceSourcePlan | None:
    """Resolve an OpenClaw JSONL trace source plan from CLI/run metadata inputs."""
    metadata = context.metadata
    instance_ids = _resolve_instance_ids(metadata)
    session_ids = _resolve_session_ids(metadata)
    profile_dirs = _resolve_profile_dirs(metadata)

    if context.run_metadata_path is not None and not session_ids:
        raise ExtractionError("OpenClaw JSONL trace collection from run metadata requires session_ids")

    profiles_root = _resolve_profiles_root(context)

    return OpenClawJsonlTraceSourcePlan(
        start_ns=context.start_ns,
        end_ns=context.end_ns,
        profiles_root=profiles_root,
        profile_dirs=profile_dirs,
        instance_ids=instance_ids,
        session_ids=session_ids,
    )


def _resolve_profiles_root(context: TraceSourceResolveContext) -> Path | None:
    raw_profiles_root = context.source_options.get("openclaw_profiles_dir")
    profiles_root = raw_profiles_root if isinstance(raw_profiles_root, Path) else None
    if profiles_root is None and context.run_metadata_path is not None:
        profiles_root = context.run_metadata_path.parent / "openclaw-profiles"
    return profiles_root


def _resolve_instance_ids(metadata: dict[str, object] | None) -> set[str] | None:
    if metadata is None:
        return None
    raw = metadata.get("instance_ids")
    if not isinstance(raw, list):
        return None
    ids = {item for item in raw if isinstance(item, str) and item}
    return ids or None


def _resolve_session_ids(metadata: dict[str, object] | None) -> set[str] | None:
    if metadata is None:
        return None
    raw = metadata.get("session_ids")
    if not isinstance(raw, dict):
        return None
    ids = {value for value in raw.values() if isinstance(value, str) and value}
    return ids or None


def _resolve_profile_dirs(metadata: dict[str, object] | None) -> list[Path] | None:
    if metadata is None:
        return None
    raw = metadata.get("openclaw_profile_dirs")
    if not isinstance(raw, dict):
        return None
    dirs = (value for value in raw.values() if isinstance(value, str) and value)
    paths = [Path(item) for item in dirs]
    return paths or None


register_trace_source_resolver(resolve_openclaw_jsonl_trace_source)
