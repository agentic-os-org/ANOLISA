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

"""Data models for evaluation results and reports."""

from __future__ import annotations

from typing import Any

from pydantic import BaseModel, Field


class EvalInstanceResult(BaseModel):
    """Evaluation result for a single instance."""

    instance_id: str
    resolved: bool = Field(description="True if RESOLVED_FULL")
    resolution_status: str = Field(description="RESOLVED_FULL / RESOLVED_PARTIAL / RESOLVED_NO")
    patch_applied: bool = Field(description="Whether patch was successfully applied")
    test_results: dict[str, Any] | None = Field(default=None, description="FAIL_TO_PASS / PASS_TO_PASS details")
    error: str | None = Field(default=None, description="Error message if evaluation failed")


class EvalReport(BaseModel):
    """Overall evaluation report."""

    total_instances: int
    resolved_full: int
    resolved_partial: int
    resolved_no: int
    patch_failed: int
    error_count: int
    resolution_rate: float = Field(description="resolved_full / total_instances")
    instance_results: list[EvalInstanceResult]
    run_id: str
    dataset_name: str
    evaluated_at: str = Field(description="ISO 8601 timestamp")
