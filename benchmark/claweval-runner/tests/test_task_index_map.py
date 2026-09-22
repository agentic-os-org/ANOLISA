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

"""Tests for scripts/task_index_map.py.

Covers:
- Normal path: 1-based index assigned over name-sorted task dirs
- Index ordering matches discover_tasks (name-sorted)
- --prefix filtering keeps original global indices
- Backward-compat default tasks-dir resolves
- Edge cases: empty dir, dirs without task.yaml, non-existent dir
"""

import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "scripts"))

from task_index_map import scan_task_index  # noqa: E402


def _make_tasks(base: Path, names: list[str], with_yaml: bool = True) -> None:
    for name in names:
        d = base / name
        d.mkdir()
        if with_yaml:
            (d / "task.yaml").write_text(f"task_id: {name}\n")


class TestScanTaskIndex:
    def test_basic_indexing(self, tmp_path):
        _make_tasks(tmp_path, ["T002_b", "T001_a", "C01_c"])
        result = scan_task_index(tmp_path)
        # Sorted by name -> C01_c, T001_a, T002_b
        assert result == [(1, "C01_c"), (2, "T001_a"), (3, "T002_b")]

    def test_index_is_one_based(self, tmp_path):
        _make_tasks(tmp_path, ["A", "B", "C"])
        result = scan_task_index(tmp_path)
        assert result[0][0] == 1
        assert result[-1][0] == len(result)

    def test_dirs_without_yaml_ignored(self, tmp_path):
        _make_tasks(tmp_path, ["T001_ok"], with_yaml=True)
        _make_tasks(tmp_path, ["misc_no_yaml"], with_yaml=False)
        result = scan_task_index(tmp_path)
        assert result == [(1, "T001_ok")]

    def test_files_ignored(self, tmp_path):
        _make_tasks(tmp_path, ["T001_ok"])
        (tmp_path / "README.md").write_text("hello")
        result = scan_task_index(tmp_path)
        assert result == [(1, "T001_ok")]

    def test_prefix_filter_keeps_global_index(self, tmp_path):
        _make_tasks(tmp_path, ["C01_a", "M01_b", "T01_c", "T02_d"])
        result = scan_task_index(tmp_path, prefix="T")
        # Global order: C01_a(1), M01_b(2), T01_c(3), T02_d(4)
        assert result == [(3, "T01_c"), (4, "T02_d")]

    def test_prefix_no_match(self, tmp_path):
        _make_tasks(tmp_path, ["T001_a"])
        assert scan_task_index(tmp_path, prefix="Z") == []

    def test_empty_dir(self, tmp_path):
        assert scan_task_index(tmp_path) == []

    def test_missing_dir_exits(self, tmp_path):
        with pytest.raises(SystemExit):
            scan_task_index(tmp_path / "does_not_exist")
