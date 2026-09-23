#!/usr/bin/env python3

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

"""Scan claw-eval tasks and print a mapping of 1-based index -> task_id.

The index follows the same ordering as ce_runner's discover_tasks (and the
claw-eval --range slice): task directories that contain a task.yaml are sorted
by directory name, then enumerated starting at 1. With 300 tasks the index
range is 1-300, so an index can be fed back into `--range N-N` to locate a task.

Usage:
    python scripts/task_index_map.py
    python scripts/task_index_map.py --prefix T
    python scripts/task_index_map.py --tasks-dir /path/to/tasks
"""

import argparse
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
TASKS_DIR = REPO_ROOT / "claw-eval" / "tasks"


def scan_task_index(tasks_dir: Path, prefix: str | None = None) -> list[tuple[int, str]]:
    """Return a list of (index, task_id) pairs.

    task_id is the task directory name. The index is the 1-based position in the
    name-sorted list of all task directories, assigned before any --prefix filter
    so that indices stay stable and match discover_tasks / --range semantics.
    """
    if not tasks_dir.is_dir():
        print(f"ERROR: tasks directory not found: {tasks_dir}", file=sys.stderr)
        sys.exit(1)

    task_ids = sorted(
        d.name for d in tasks_dir.iterdir()
        if d.is_dir() and (d / "task.yaml").exists()
    )

    mapping = list(enumerate(task_ids, start=1))
    if prefix:
        mapping = [(i, name) for i, name in mapping if name.startswith(prefix)]
    return mapping


def print_mapping(mapping: list[tuple[int, str]]) -> None:
    """Print the index -> task_id mapping as a tab-separated table."""
    for index, task_id in mapping:
        print(f"{index}\t{task_id}")


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Print a mapping of 1-based index -> task_id for claw-eval tasks",
        epilog="Index ordering matches ce_runner discover_tasks (name-sorted, 1-based).",
    )
    parser.add_argument(
        "--tasks-dir",
        type=Path,
        default=TASKS_DIR,
        help=f"Directory containing task subdirectories (default: {TASKS_DIR})",
    )
    parser.add_argument(
        "--prefix",
        help="Only print tasks whose name starts with this prefix (e.g. T, M, C). "
             "Indices are still assigned over all tasks before filtering.",
    )
    args = parser.parse_args()

    mapping = scan_task_index(args.tasks_dir, prefix=args.prefix)
    if not mapping:
        print("No tasks found matching the filters.", file=sys.stderr)
        return
    print_mapping(mapping)


if __name__ == "__main__":
    main()
