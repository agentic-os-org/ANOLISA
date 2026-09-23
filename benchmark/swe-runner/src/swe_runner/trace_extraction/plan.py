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

"""Trace collection plan resolution and execution."""

from __future__ import annotations

import json
import logging
from pathlib import Path
from typing import Any, cast

from pydantic import BaseModel

from swe_runner.trace_extraction.helpers import ExtractionError, parse_time_value
from swe_runner.trace_extraction.sources import TraceSourcePlan, TraceSourceResolveContext, resolve_trace_source_plan

logger = logging.getLogger(__name__)


class TraceCollectionPlan(BaseModel):
    """Resolved plan for collecting traces from OpenClaw JSONL sessions.

    Use ``TraceCollectionPlan.resolve(...)`` to build from raw CLI inputs.
    When ``should_collect`` is False, calling ``collect()`` is a no-op.
    """

    should_collect: bool
    start_ns: int = 0
    end_ns: int = 0
    profiles_root: Path | None = None
    profile_dirs: list[Path] | None = None
    instance_ids: set[str] | None = None
    session_ids: set[str] | None = None
    source_name: str | None = None
    source_plan: Any = None

    model_config = {"arbitrary_types_allowed": True}

    @classmethod
    def resolve(
        cls,
        *,
        start: str | None = None,
        end: str = "now",
        run_metadata_path: Path | None = None,
        openclaw_profiles_dir: Path | None = None,
    ) -> TraceCollectionPlan:
        """Resolve raw CLI inputs into a validated trace collection plan.

        Raises ExtractionError on invalid input combinations.
        """
        metadata = _load_metadata(run_metadata_path)

        resolved_start_ns = _resolve_start_ns(start, metadata)

        should_collect = resolved_start_ns is not None or run_metadata_path is not None
        if not should_collect and end != "now":
            raise ExtractionError("--end requires --start or --run-metadata")

        if not should_collect:
            return cls(should_collect=False)

        resolved_end_ns = _resolve_end_ns(end, metadata)

        if resolved_start_ns is None:
            raise ExtractionError("Unable to determine trace window start time")
        if resolved_end_ns < resolved_start_ns:
            raise ExtractionError("Trace window end must be greater than or equal to start")

        source_plan = resolve_trace_source_plan(
            TraceSourceResolveContext(
                start_ns=resolved_start_ns,
                end_ns=resolved_end_ns,
                metadata=metadata,
                run_metadata_path=run_metadata_path,
                source_options={"openclaw_profiles_dir": openclaw_profiles_dir},
            )
        )
        if source_plan is None:
            raise ExtractionError("Unable to resolve a trace source for the requested collection")

        return cls(
            should_collect=True,
            start_ns=resolved_start_ns,
            end_ns=resolved_end_ns,
            profiles_root=getattr(source_plan, "profiles_root", None),
            profile_dirs=getattr(source_plan, "profile_dirs", None),
            instance_ids=getattr(source_plan, "instance_ids", None),
            session_ids=getattr(source_plan, "session_ids", None),
            source_name=source_plan.name,
            source_plan=source_plan,
        )

    def collect(self, trace_root: Path) -> list[Path] | None:
        """Execute trace collection. Returns None if should_collect is False."""
        if not self.should_collect:
            return None

        if self.source_plan is None:
            raise ExtractionError("Trace collection plan is missing a trace source")
        return cast(TraceSourcePlan, self.source_plan).collect(trace_root)


def _load_metadata(run_metadata_path: Path | None) -> dict[str, object] | None:
    if run_metadata_path is None:
        return None
    try:
        payload = json.loads(run_metadata_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError, ValueError) as exc:
        raise ExtractionError(f"Failed to load run metadata: {run_metadata_path}") from exc
    if not isinstance(payload, dict):
        raise ExtractionError(f"Run metadata must be a JSON object: {run_metadata_path}")
    return cast(dict[str, object], payload)


def _resolve_start_ns(start: str | None, metadata: dict[str, object] | None) -> int | None:
    if start is not None:
        return parse_time_value(start)
    if metadata is not None:
        raw = metadata.get("started_at_ns")
        if isinstance(raw, int):
            return raw
    return None


def _resolve_end_ns(end: str, metadata: dict[str, object] | None) -> int:
    if end != "now":
        return parse_time_value(end)
    if metadata is not None:
        raw = metadata.get("ended_at_ns")
        if isinstance(raw, int):
            return raw + 10_000_000_000
    return parse_time_value("now")
