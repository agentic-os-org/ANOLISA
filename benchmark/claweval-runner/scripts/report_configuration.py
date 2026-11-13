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

"""Model-runtime-independent settings policy for standalone report tools."""

from __future__ import annotations

from pathlib import Path
from typing import Any

import yaml


def resolve_report_configuration(
    config_path: str | None,
    *,
    repository_dir: Path,
    trace_dir: str | None = None,
    tasks_dir: str | None = None,
    judge_model: str | None = None,
    judge_base_url: str | None = None,
    judge_api_key: str | None = None,
    output_dir: str | None = None,
    include_output_dir: bool = False,
) -> dict[str, Any]:
    """Resolve report settings with explicit values taking precedence over YAML.

    Include an output directory only for the detail reporter. Keep command-line
    namespaces and model-client construction outside this shared policy.
    """
    result: dict[str, Any] = {"trace_dir": None, "tasks_dir": None}
    if include_output_dir:
        result["output_dir"] = None
    result.update(judge_api_key="", judge_base_url="", judge_model_id="")

    if config_path:
        config_file = Path(config_path)
        with config_file.open() as handle:
            config = yaml.safe_load(handle) or {}
        judge = config.get("judge", {})
        defaults = config.get("defaults", {})
        result["judge_api_key"] = judge.get("api_key", "")
        result["judge_base_url"] = judge.get("base_url", "")
        result["judge_model_id"] = judge.get("model_id", "")
        result["trace_dir"] = str(
            config_file.parent / defaults.get("trace_dir", "traces")
        )
        result["tasks_dir"] = str(
            config_file.parent / defaults.get("tasks_dir", "tasks")
        )
        if include_output_dir:
            result["output_dir"] = str(config_file.parent / "reports")

    overrides = {
        "trace_dir": trace_dir,
        "tasks_dir": tasks_dir,
        "judge_model_id": judge_model,
        "judge_base_url": judge_base_url,
        "judge_api_key": judge_api_key,
    }
    if include_output_dir:
        overrides["output_dir"] = output_dir
    result.update({key: value for key, value in overrides.items() if value})

    for key, directory in (
        ("trace_dir", "traces"),
        ("tasks_dir", "tasks"),
        ("output_dir", "reports"),
    ):
        if key in result and result[key] is None:
            result[key] = str(repository_dir / "claw-eval" / directory)
    return result
