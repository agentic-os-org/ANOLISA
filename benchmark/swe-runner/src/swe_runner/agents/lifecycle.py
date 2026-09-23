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

"""Shared agent lifecycle primitives."""

from __future__ import annotations

import logging
from abc import ABC, abstractmethod
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path

from swe_runner.common.models import AgentResult, Settings, SWEInstance

logger = logging.getLogger(__name__)


@dataclass
class PreparedAgentRun:
    """State produced by an agent's prepare phase and consumed by run/post."""

    instance: SWEInstance
    settings: Settings
    work_dir: Path
    prompt: str
    timeout: int
    max_turns: int
    base_revision: str | None = None
    metadata: dict[str, str] = field(default_factory=dict)
    cleanup_callbacks: list[Callable[[], None]] = field(default_factory=list)

    def cleanup(self) -> None:
        """Run cleanup callbacks in reverse registration order."""
        while self.cleanup_callbacks:
            callback = self.cleanup_callbacks.pop()
            try:
                callback()
            except Exception:
                logger.exception("AGENT_CLEANUP_FAILED instance=%s", self.instance.instance_id)


class AgentAdapter(ABC):
    """Base class for external agent runner adapters."""

    @property
    @abstractmethod
    def name(self) -> str:
        """External agent name used in CLI/config output."""

    def prepare_batch(self, instances: list[SWEInstance], settings: Settings) -> None:
        """Prepare shared batch resources before instances are dispatched."""
        del instances, settings

    @abstractmethod
    def prepare(self, instance: SWEInstance, settings: Settings) -> PreparedAgentRun:
        """Prepare the runtime environment and prompt for one instance."""

    @abstractmethod
    def run(self, prepared: PreparedAgentRun) -> AgentResult:
        """Execute the external agent runner against a prepared instance."""

    def post_batch(self) -> None:
        """Clean up shared batch resources after all instances finish."""
        return None
