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

"""List and group tasks under claw-eval/tasks/ by prefix (T/M/C) and difficulty.

Usage:
    python scripts/list_tasks.py
    python scripts/list_tasks.py --prefix T
    python scripts/list_tasks.py --difficulty hard
"""

import argparse
import sys
from collections import defaultdict
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
TASKS_DIR = REPO_ROOT / "claw-eval" / "tasks"


def _load_yaml_simple(path: Path) -> dict:
    """Minimal YAML parser for flat task.yaml files (no external deps)."""
    import re

    result = {}
    with open(path) as f:
        for line in f:
            line = line.rstrip()
            if not line or line.startswith("#"):
                continue
            # Stop at nested structures (prompt:, tools:, services:, etc.)
            if re.match(r"^\w", line) and ":" in line:
                key, _, value = line.partition(":")
                value = value.strip()
                # Strip quotes
                if value and value[0] in ('"', "'"):
                    value = value.strip("\"'")
                result[key.strip()] = value
            elif not line.startswith(" ") and not line.startswith("\t"):
                # Top-level key with nested value (e.g. "prompt:")
                key = line.rstrip(":")
                if key not in result:
                    result[key] = ""
    return result


def scan_tasks(prefix_filter: str | None = None,
               difficulty_filter: str | None = None) -> list[dict]:
    """Scan task directories and return list of task info dicts."""
    if not TASKS_DIR.exists():
        print(f"ERROR: tasks directory not found: {TASKS_DIR}", file=sys.stderr)
        sys.exit(1)

    tasks = []
    for d in sorted(TASKS_DIR.iterdir()):
        if not d.is_dir():
            continue
        task_yaml = d / "task.yaml"
        if not task_yaml.exists():
            continue

        meta = _load_yaml_simple(task_yaml)
        task_id = meta.get("task_id", d.name)
        difficulty = meta.get("difficulty", "unknown")
        # Determine prefix from task_id
        prefix = (task_id[0] if task_id else "?").upper()

        if prefix_filter and prefix != prefix_filter.upper():
            continue
        if difficulty_filter and difficulty != difficulty_filter:
            continue

        tasks.append({
            "task_id": task_id,
            "task_name": meta.get("task_name", ""),
            "difficulty": difficulty,
            "prefix": prefix,
            "category": meta.get("category", ""),
            "tags": meta.get("tags", ""),
        })

    return tasks


def print_grouped(tasks: list[dict]):
    """Print tasks grouped by prefix and difficulty."""
    # Group by (prefix, difficulty)
    groups = defaultdict(list)
    for t in tasks:
        groups[(t["prefix"], t["difficulty"])].append(t)

    # Define display order
    prefix_order = ["T", "M", "C"]
    difficulty_order = ["simple", "easy", "medium", "hard", "expert"]

    # Collect all prefixes/difficulties present and sort
    prefixes = sorted({p for p, _ in groups},
                      key=lambda x: prefix_order.index(x) if x in prefix_order else 99)
    difficulties = sorted({d for _, d in groups},
                          key=lambda x: difficulty_order.index(x) if x in difficulty_order else 99)

    total = 0
    for prefix in prefixes:
        prefix_count = 0
        print(f"\n{'='*60}")
        print(f"  Prefix: {prefix}")
        print(f"{'='*60}")

        for difficulty in difficulties:
            items = groups.get((prefix, difficulty), [])
            if not items:
                continue
            count = len(items)
            prefix_count += count
            total += count
            print(f"\n  [{difficulty}] ({count})")
            for item in items:
                print(f"    {item['task_id']}")

        print(f"\n  --- Prefix {prefix} total: {prefix_count}")

    print(f"\n{'='*60}")
    print(f"  Grand total: {total} tasks")
    print(f"{'='*60}\n")


def main():
    parser = argparse.ArgumentParser(
        description="List and group claw-eval tasks by prefix and difficulty",
    )
    parser.add_argument("--prefix", choices=["T", "M", "C"],
                        help="Filter by task prefix (T, M, or C)")
    parser.add_argument("--difficulty",
                        choices=["simple", "easy", "medium", "hard", "expert"],
                        help="Filter by difficulty")

    args = parser.parse_args()

    tasks = scan_tasks(prefix_filter=args.prefix, difficulty_filter=args.difficulty)
    if not tasks:
        print("No tasks found matching the filters.")
        return

    print_grouped(tasks)


if __name__ == "__main__":
    main()
