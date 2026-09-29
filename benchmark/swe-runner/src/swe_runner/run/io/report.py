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

"""Aggregated run reports."""

from __future__ import annotations

from pathlib import Path

from pydantic import BaseModel, Field

from swe_runner.agents.metadata import collect_result_metadata
from swe_runner.common.models import InstanceResult


class RunReport(BaseModel):
    """Aggregated report produced by a completed run session."""

    succeeded: int
    failed: int
    total: int
    instance_ids: list[str]
    metadata_mappings: dict[str, dict[str, str]] = Field(default_factory=dict)
    metadata_path: Path | None = None
    started_at_ns: int = 0
    ended_at_ns: int = 0

    model_config = {"arbitrary_types_allowed": True}

    @classmethod
    def from_results(
        cls,
        results: list[InstanceResult],
        *,
        started_at_ns: int,
        ended_at_ns: int,
        metadata_path: Path | None = None,
    ) -> RunReport:
        """Build a RunReport by aggregating a list of InstanceResults."""
        succeeded = sum(1 for result in results if result.success)
        instance_ids = [result.instance.instance_id for result in results]
        metadata_mappings: dict[str, dict[str, str]] = {}

        for result in results:
            collected_metadata = collect_result_metadata(result.agent_result.metadata)
            for key, value in collected_metadata.run_metadata_mappings.items():
                metadata_mappings.setdefault(key, {})[result.instance.instance_id] = value

        return cls(
            succeeded=succeeded,
            failed=len(results) - succeeded,
            total=len(results),
            instance_ids=instance_ids,
            metadata_mappings=metadata_mappings,
            metadata_path=metadata_path,
            started_at_ns=started_at_ns,
            ended_at_ns=ended_at_ns,
        )
