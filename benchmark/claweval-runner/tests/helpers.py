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

"""Shared test utilities for claw-eval trace tests."""

from pathlib import Path


TRACES_DIR = Path(__file__).resolve().parent.parent / "claw-eval" / "traces"


def discover_task_ids(base_dir: Path) -> list[str]:
    """Discover all unique task IDs from trace filenames in base_dir.

    Trace filenames follow the pattern: {task_id}_{uuid}.jsonl
    Returns sorted list of unique task IDs.
    """
    task_ids: set[str] = set()
    if not base_dir.exists():
        return []
    for subdir in base_dir.iterdir():
        if not subdir.is_dir():
            continue
        for f in subdir.glob("*.jsonl"):
            stem = f.stem
            # UUID part is the last segment after underscore, 8+ hex chars
            parts = stem.rsplit("_", 1)
            if len(parts) == 2 and len(parts[1]) >= 8:
                task_ids.add(parts[0])
    return sorted(task_ids)


def find_latest_trace(base_dir: Path, keyword: str, dir_pattern: str) -> tuple[Path | None, str]:
    """Find the most recent trace file matching keyword in directories matching dir_pattern.

    Args:
        base_dir: Base traces directory
        keyword: Task ID to match in filename (e.g. "T009zh_contact_lookup")
        dir_pattern: Directory name prefix to match (e.g. "openclaw" or empty for native)

    Returns:
        (trace_path, directory_name) or (None, "")
    """
    candidates: list[tuple[float, Path, str]] = []  # (mtime, path, dir_name)

    if not base_dir.exists():
        return None, ""

    for subdir in base_dir.iterdir():
        if not subdir.is_dir():
            continue

        # Match directory pattern
        if dir_pattern:
            if not subdir.name.startswith(dir_pattern):
                continue
        else:
            # Native traces: exclude openclaw_* directories
            if subdir.name.startswith("openclaw"):
                continue

        # Find matching files
        for f in subdir.glob(f"*{keyword}*.jsonl"):
            if f.is_file():
                candidates.append((f.stat().st_mtime, f, subdir.name))

    if not candidates:
        return None, ""

    # Return the file with the latest modification time
    candidates.sort(key=lambda x: x[0], reverse=True)
    mtime, latest_path, dir_name = candidates[0]
    return latest_path, dir_name
