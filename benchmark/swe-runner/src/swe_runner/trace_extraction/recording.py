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

"""Top-level orchestration for recording OpenClaw local session traces."""

import json
import logging
import re
from collections.abc import Iterable
from pathlib import Path
from typing import Any

from swe_runner.trace_extraction.openclaw_jsonl import (
    DEFAULT_OPENCLAW_PROFILES_DIR,
    iter_openclaw_jsonl_traces,
)

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

DEFAULT_TRACE_OUTPUT_DIR = Path("traces")


# ---------------------------------------------------------------------------
# File-writing helpers (formerly recorder.py)
# ---------------------------------------------------------------------------


def _next_trace_file(issue_dir: Path) -> Path:
    """Return the next traceN.json path under an instance trace directory."""
    max_index = 0
    for path in issue_dir.glob("trace*.json"):
        match = re.fullmatch(r"trace(\d+)\.json", path.name)
        if match:
            max_index = max(max_index, int(match.group(1)))
    return issue_dir / f"trace{max_index + 1}.json"


def _write_trace_file(instance_id: str, trace_root: str | Path, session_data: dict[str, Any]) -> Path:
    """Write one session trace under the instance trace directory."""
    from swe_runner.trace_extraction.helpers import sanitize_path_component

    issue_dir = Path(trace_root) / sanitize_path_component(instance_id)
    issue_dir.mkdir(parents=True, exist_ok=True)
    trace_file = _next_trace_file(issue_dir)
    with open(trace_file, "w", encoding="utf-8") as f:
        json.dump(session_data, f, indent=2, ensure_ascii=False)
    return trace_file


# ---------------------------------------------------------------------------
# Public orchestration API
# ---------------------------------------------------------------------------


class OpenClawJsonlTraceRecorder:
    """Trace recorder backed by local OpenClaw profile session JSONL transcripts."""

    def __init__(
        self,
        *,
        profiles_root: str | Path | None = DEFAULT_OPENCLAW_PROFILES_DIR,
        profile_dirs: Iterable[str | Path] | None = None,
    ) -> None:
        self._profiles_root = profiles_root
        self._profile_dirs = profile_dirs

    def record_window(
        self,
        *,
        start_ns: int,
        end_ns: int,
        trace_root: str | Path,
        instance_ids: set[str] | None = None,
        session_ids: set[str] | None = None,
    ) -> list[Path]:
        """Record OpenClaw JSONL traces by explicit session id or time window."""
        recorded: list[Path] = []
        for trace_data in iter_openclaw_jsonl_traces(
            profiles_root=self._profiles_root,
            profile_dirs=self._profile_dirs,
            start_ns=start_ns,
            end_ns=end_ns,
            instance_ids=instance_ids,
            session_ids=session_ids,
        ):
            instance_id = trace_data.get("issue_id")
            if not isinstance(instance_id, str) or not instance_id:
                continue
            recorded.append(_write_trace_file(instance_id, trace_root, trace_data))
        return recorded


def record_openclaw_jsonl_traces_in_window(
    start_ns: int,
    end_ns: int,
    profiles_root: str | Path | None = DEFAULT_OPENCLAW_PROFILES_DIR,
    profile_dirs: Iterable[str | Path] | None = None,
    trace_root: str | Path = DEFAULT_TRACE_OUTPUT_DIR,
    instance_ids: set[str] | None = None,
    session_ids: set[str] | None = None,
) -> list[Path]:
    """Record OpenClaw session JSONL traces into runner trace JSON files."""
    if end_ns < start_ns:
        raise ValueError(f"end_ns must be >= start_ns, got start_ns={start_ns}, end_ns={end_ns}")
    recorder = OpenClawJsonlTraceRecorder(profiles_root=profiles_root, profile_dirs=profile_dirs)
    return recorder.record_window(
        start_ns=start_ns,
        end_ns=end_ns,
        trace_root=trace_root,
        instance_ids=instance_ids,
        session_ids=session_ids,
    )
