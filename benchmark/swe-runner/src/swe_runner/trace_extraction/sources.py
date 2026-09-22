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

"""Trace source resolver registry."""

from __future__ import annotations

import importlib
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Protocol


class TraceSourcePlan(Protocol):
    """Executable plan for one trace source."""

    @property
    def name(self) -> str:
        """Trace source name."""

    def collect(self, trace_root: Path) -> list[Path]:
        """Collect traces into ``trace_root`` and return written trace files."""


@dataclass(frozen=True)
class TraceSourceResolveContext:
    """Inputs available when resolving a concrete trace source."""

    start_ns: int
    end_ns: int
    metadata: dict[str, object] | None = None
    run_metadata_path: Path | None = None
    source_options: dict[str, object] = field(default_factory=dict)


TraceSourceResolver = Callable[[TraceSourceResolveContext], TraceSourcePlan | None]

_SOURCE_MODULES = (
    "swe_runner.trace_extraction.openclaw_source",
)
_REGISTERED_RESOLVERS: list[TraceSourceResolver] = []
_SOURCE_MODULES_LOADED = False


def register_trace_source_resolver(resolver: TraceSourceResolver) -> None:
    """Register a trace source resolver."""
    if resolver not in _REGISTERED_RESOLVERS:
        _REGISTERED_RESOLVERS.append(resolver)


def resolve_trace_source_plan(context: TraceSourceResolveContext) -> TraceSourcePlan | None:
    """Resolve the first trace source that can satisfy the context."""
    _load_source_modules()
    for resolver in _REGISTERED_RESOLVERS:
        source_plan = resolver(context)
        if source_plan is not None:
            return source_plan
    return None


def _load_source_modules() -> None:
    global _SOURCE_MODULES_LOADED
    if _SOURCE_MODULES_LOADED:
        return
    for module_name in _SOURCE_MODULES:
        importlib.import_module(module_name)
    _SOURCE_MODULES_LOADED = True
