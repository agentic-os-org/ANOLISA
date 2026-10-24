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

"""Filesystem-based task resolution shared by runner and interactive scripts."""

from pathlib import Path


def resolve_task_path(
    task_path: str | Path, *, tasks_dir: Path | None = None
) -> tuple[str, str]:
    """Resolve a task file/directory, optionally falling back to a default root.

    Existing local paths take precedence. Return absolute task-file and task-dir
    paths; callers retain their own logging and process-exit policy.
    """
    path = Path(task_path)
    if not path.exists() and tasks_dir is not None:
        path = tasks_dir / path
    if path.is_dir():
        task_yaml = path / "task.yaml"
        if not task_yaml.is_file():
            raise ValueError(f"task.yaml not found in {path}")
    elif path.is_file():
        task_yaml = path
    else:
        raise ValueError(f"task path not found: {task_path}")
    return str(task_yaml.resolve()), str(task_yaml.parent.resolve())
