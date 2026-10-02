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

"""Core domain and configuration models for SWE-bench runner."""

from __future__ import annotations

from collections.abc import Mapping
from pathlib import Path
from typing import Any

from pydantic import BaseModel, Field, model_validator


class SWEInstance(BaseModel):
    """Represents a single SWE-bench task instance."""

    instance_id: str
    repo: str
    version: str
    base_commit: str
    problem_statement: str
    patch: str
    test_patch: str
    image_name: str | None = None
    docker_image: str | None = None

    @classmethod
    def from_dataset_row(cls, row: Mapping[str, Any]) -> SWEInstance:
        """Construct SWEInstance from HuggingFace dataset dict."""
        return cls(
            instance_id=row["instance_id"],
            repo=row["repo"],
            version=row["version"],
            base_commit=row["base_commit"],
            problem_statement=row["problem_statement"],
            patch=row["patch"],
            test_patch=row["test_patch"],
            image_name=row.get("image_name"),
            docker_image=row.get("docker_image"),
        )


class Prediction(BaseModel):
    """Output prediction for SWE-bench evaluation."""

    instance_id: str
    model_name_or_path: str
    model_patch: str


class AgentResult(BaseModel):
    """Result from agent invocation."""

    raw_output: str
    patch: str | None
    success: bool
    duration_seconds: float
    error: str | None = None
    metadata: dict[str, str] = Field(default_factory=dict)


class InstanceResult(BaseModel):
    """Combined result for a single instance."""

    instance: SWEInstance
    prediction: Prediction | None
    agent_result: AgentResult
    success: bool


class AgentConfig(BaseModel):
    """Agent configuration."""

    name: str = Field(description="Agent name (e.g., 'cosh', 'openclaw')")
    timeout: int = Field(default=1800, description="Timeout in seconds")
    step_limit: int = Field(default=0, description="Max steps (0 = unlimited)")
    workers: int = Field(default=1, ge=1, description="Number of parallel workers")
    docker_pull_registry: str | None = Field(
        default=None,
        description="Optional registry host used as the source for docker pull",
    )
    use_skill: bool = Field(
        default=False,
        description="Whether to enable optional SWE-bench skill guidance for the agent",
    )
    skills_dir: Path | None = Field(
        default=None,
        description="Optional directory containing skills laid out as <skill-name>/SKILL.md",
    )
    tokenless: bool = Field(
        default=False,
        description="Whether to enable tokenless/rtk helpers when the selected agent supports them",
    )
    per_case_prompt: bool = Field(
        default=False,
        description="Whether to enable optional per-instance custom prompt guidance",
    )
    prompts_dir: Path | None = Field(
        default=None,
        description="Optional directory containing per-instance prompt files named by instance_id",
    )

    @model_validator(mode="after")
    def validate_guidance_mode(self) -> AgentConfig:
        if self.use_skill and self.per_case_prompt:
            raise ValueError("use_skill and per_case_prompt are mutually exclusive")
        return self


class DatasetConfig(BaseModel):
    """Dataset loading configuration."""

    subset: str = Field(
        default="lite",
        description="SWE-bench subset name (lite/verified/full/multilingual or custom path)",
    )
    split: str = Field(default="dev", description="Dataset split")
    filter_regex: str | None = Field(default=None, description="Regex filter for instance IDs")
    slice_range: str | None = Field(default=None, description="Slice string (e.g., '0:5')")
    instance_ids: list[str] | None = Field(default=None, description="Specific instance IDs to run")

    def get_slice(self) -> tuple[int, int] | None:
        """Parse slice_range string to (start, end) tuple.

        Returns:
            Tuple of (start, end) or None if slice_range is None.

        Examples:
            "0:5" -> (0, 5)
            "10:" -> (10, -1)
            ":5" -> (0, 5)
            None -> None
        """
        if self.slice_range is None:
            return None

        parts = self.slice_range.split(":")
        if len(parts) != 2:
            return None

        start_str, end_str = parts
        start = int(start_str) if start_str else 0
        end = int(end_str) if end_str else -1

        return (start, end)


class OutputConfig(BaseModel):
    """Output configuration."""

    output_dir: Path = Field(default=Path("./output"), description="Output directory")


class Settings(BaseModel):
    """Top-level settings combining all configs."""

    agent: AgentConfig = Field(default_factory=lambda: AgentConfig(name="cosh"))
    dataset: DatasetConfig = Field(default_factory=DatasetConfig)
    output: OutputConfig = Field(default_factory=OutputConfig)
