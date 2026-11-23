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

"""Common task and trace evidence for the standalone reporting scripts."""

import json
import os
from typing import Any

import yaml


def load_task_info(task_id: str, tasks_dir: str) -> dict[str, Any]:
    """Project task metadata while retaining the reports' fallback and limits."""
    yaml_path = os.path.join(tasks_dir, task_id, "task.yaml")
    if not os.path.exists(yaml_path):
        return {"task_id": task_id, "error": "task.yaml not found"}
    with open(yaml_path) as file:
        data = yaml.safe_load(file)
    return {
        "task_id": data.get("task_id", task_id),
        "task_name": data.get("task_name", ""),
        "category": data.get("category", ""),
        "difficulty": data.get("difficulty", ""),
        "prompt": data.get("prompt", {}).get("text", "")[:300],
        "scoring_components": data.get("scoring_components", []),
        "judge_rubric": data.get("judge_rubric", ""),
        "reference_solution": data.get("reference_solution", "")[:300],
        "primary_dimensions": data.get("primary_dimensions", []),
    }


def load_grading_result(
    trace_path: str,
) -> tuple[dict[str, Any] | None, dict[str, Any] | None]:
    """Read the last grading and trace-end events independently from JSONL."""
    grading = None
    trace_end = None
    with open(trace_path) as file:
        for line in file:
            event = json.loads(line)
            if event.get("type") == "grading_result":
                grading = event
            if event.get("type") == "trace_end":
                trace_end = event
    return grading, trace_end
