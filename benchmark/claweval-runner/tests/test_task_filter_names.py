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

from pathlib import Path

import pytest
from ce_runner.run_task import discover_tasks


def _make_tasks(root: Path) -> None:
    root.mkdir(parents=True)
    for name in ("T001_email", "T002_calendar", "M001_clock"):
        task_dir = root / name
        task_dir.mkdir()
        (task_dir / "task.yaml").write_text(f"task_id: {name}\ntags: [general]\n")


@pytest.mark.parametrize("parent", ["email-bench", "EMAIL-bench"])
def test_matching_ancestor_does_not_select_unrelated_tasks(
    tmp_path: Path, parent: str
) -> None:
    root = tmp_path / parent / "tasks"
    _make_tasks(root)
    selected = discover_tasks(str(root), filter_str="email")
    assert [Path(path).name for path in selected] == ["T001_email"]


def test_path_separator_does_not_match_task_name(tmp_path: Path) -> None:
    root = tmp_path / "dataset" / "tasks"
    _make_tasks(root)
    assert discover_tasks(str(root), filter_str="tasks/") == []


def test_filter_composes_with_prefix_tag_and_range(tmp_path: Path) -> None:
    root = tmp_path / "calendar" / "tasks"
    _make_tasks(root)
    selected = discover_tasks(
        str(root), prefix="T", filter_str="EMAIL", tag="general", range_str="1-1"
    )
    assert [Path(path).name for path in selected] == ["T001_email"]
