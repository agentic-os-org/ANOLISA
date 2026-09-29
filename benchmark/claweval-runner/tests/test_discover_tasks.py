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

"""Test task discovery and filtering logic.

Covers:
- --prefix exact prefix match
- --filter substring match (case-insensitive)
- --tag exact tag match
- --range numeric ID range (T/C/M prefixes)
- Combined filters
- Edge cases (invalid range, empty directory)

Task type coverage:
- T tasks: T001-T010 range test
- M tasks: M001-M010 prefix test
- C tasks: C01-C10 tag=user_agent test
"""

import sys
from pathlib import Path
import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))


class TestDiscoverTasksPrefix:
    """Test --prefix filtering."""

    def test_prefix_t(self, tmp_path):
        """Filter T tasks by prefix."""
        from ce_runner.run_task import discover_tasks
        
        # Create mock task directories
        for name in ["T001_test", "T002_test", "M001_test", "C01_test"]:
            task_dir = tmp_path / name
            task_dir.mkdir()
            (task_dir / "task.yaml").write_text(f"task_id: {name}\ntags: [general]\n")
        
        results = discover_tasks(str(tmp_path), prefix="T")
        assert len(results) == 2
        assert all(Path(r).name.startswith("T") for r in results)

    def test_prefix_m(self, tmp_path):
        """Filter M tasks by prefix."""
        from ce_runner.run_task import discover_tasks
        
        for name in ["M001_clock", "M002_timer", "T001_test"]:
            task_dir = tmp_path / name
            task_dir.mkdir()
            (task_dir / "task.yaml").write_text(f"task_id: {name}\ntags: [multimodal]\n")
        
        results = discover_tasks(str(tmp_path), prefix="M")
        assert len(results) == 2

    def test_prefix_c(self, tmp_path):
        """Filter C tasks by prefix."""
        from ce_runner.run_task import discover_tasks
        
        for name in ["C01_mortgage", "C02_finance", "T001_test"]:
            task_dir = tmp_path / name
            task_dir.mkdir()
            (task_dir / "task.yaml").write_text(f"task_id: {name}\ntags: [user_agent]\n")
        
        results = discover_tasks(str(tmp_path), prefix="C")
        assert len(results) == 2


class TestDiscoverTasksFilter:
    """Test --filter substring matching."""

    def test_filter_case_insensitive(self, tmp_path):
        """Filter is case-insensitive."""
        from ce_runner.run_task import discover_tasks
        
        for name in ["T001zh_email", "T002EN_sms", "M001_clock"]:
            task_dir = tmp_path / name
            task_dir.mkdir()
            (task_dir / "task.yaml").write_text(f"task_id: {name}\n")
        
        results = discover_tasks(str(tmp_path), filter_str="zh")
        assert len(results) == 1
        assert "zh" in Path(results[0]).name.lower()

    def test_filter_partial_match(self, tmp_path):
        """Filter matches substring."""
        from ce_runner.run_task import discover_tasks
        
        for name in ["T001_email_triage", "T002_email_sort", "M001_clock"]:
            task_dir = tmp_path / name
            task_dir.mkdir()
            (task_dir / "task.yaml").write_text(f"task_id: {name}\n")
        
        results = discover_tasks(str(tmp_path), filter_str="email")
        assert len(results) == 2


class TestDiscoverTasksTag:
    """Test --tag exact matching."""

    def test_tag_general(self, tmp_path):
        """Filter by general tag (T + C tasks)."""
        from ce_runner.run_task import discover_tasks
        
        tasks = [
            ("T001_test", ["general"]),
            ("C01_chat", ["general", "user_agent"]),
            ("M001_clock", ["multimodal"])
        ]
        for name, tags in tasks:
            task_dir = tmp_path / name
            task_dir.mkdir()
            (task_dir / "task.yaml").write_text(f"task_id: {name}\ntags:\n{chr(10).join('  - ' + t for t in tags)}\n")
        
        results = discover_tasks(str(tmp_path), tag="general")
        assert len(results) == 2

    def test_tag_multimodal(self, tmp_path):
        """Filter by multimodal tag (M tasks only)."""
        from ce_runner.run_task import discover_tasks
        
        tasks = [
            ("M001_clock", ["multimodal"]),
            ("M002_timer", ["multimodal"]),
            ("T001_test", ["general"])
        ]
        for name, tags in tasks:
            task_dir = tmp_path / name
            task_dir.mkdir()
            (task_dir / "task.yaml").write_text(f"task_id: {name}\ntags:\n{chr(10).join('  - ' + t for t in tags)}\n")
        
        results = discover_tasks(str(tmp_path), tag="multimodal")
        assert len(results) == 2
        assert all(Path(r).name.startswith("M") for r in results)

    def test_tag_user_agent(self, tmp_path):
        """Filter by user_agent tag (C tasks only)."""
        from ce_runner.run_task import discover_tasks
        
        tasks = [
            ("C01_mortgage", ["general", "user_agent"]),
            ("C02_finance", ["user_agent"]),
            ("T001_test", ["general"])
        ]
        for name, tags in tasks:
            task_dir = tmp_path / name
            task_dir.mkdir()
            (task_dir / "task.yaml").write_text(f"task_id: {name}\ntags:\n{chr(10).join('  - ' + t for t in tags)}\n")
        
        results = discover_tasks(str(tmp_path), tag="user_agent")
        assert len(results) == 2
        assert all(Path(r).name.startswith("C") for r in results)


class TestDiscoverTasksRange:
    """Test --range numeric ID filtering."""

    def test_range_t_tasks(self, tmp_path):
        """Filter T tasks by range."""
        from ce_runner.run_task import discover_tasks
        
        for i in range(1, 15):
            name = f"T{i:03d}_test"
            task_dir = tmp_path / name
            task_dir.mkdir()
            (task_dir / "task.yaml").write_text(f"task_id: {name}\n")
        
        results = discover_tasks(str(tmp_path), range_str="1-10")
        assert len(results) == 10

    def test_range_with_step(self, tmp_path):
        """Range with step subsamples every Nth task."""
        from ce_runner.run_task import discover_tasks

        for i in range(1, 15):
            name = f"T{i:03d}_test"
            task_dir = tmp_path / name
            task_dir.mkdir()
            (task_dir / "task.yaml").write_text(f"task_id: {name}\n")

        results = discover_tasks(str(tmp_path), range_str="1-10:2")
        names = sorted(Path(r).name for r in results)
        assert names == ["T001_test", "T003_test", "T005_test", "T007_test", "T009_test"]

    def test_range_step_one_equals_no_step(self, tmp_path):
        """Step of 1 is equivalent to omitting the step (backward compatible)."""
        from ce_runner.run_task import discover_tasks

        for i in range(1, 15):
            name = f"T{i:03d}_test"
            task_dir = tmp_path / name
            task_dir.mkdir()
            (task_dir / "task.yaml").write_text(f"task_id: {name}\n")

        assert (discover_tasks(str(tmp_path), range_str="1-10:1")
                == discover_tasks(str(tmp_path), range_str="1-10"))

    def test_range_invalid_step(self, tmp_path):
        """Step of 0 should exit."""
        from ce_runner.run_task import discover_tasks

        with pytest.raises(SystemExit):
            discover_tasks(str(tmp_path), range_str="1-10:0")

    def test_range_m_tasks_with_prefix(self, tmp_path):
        """Filter M tasks by range with --prefix."""
        from ce_runner.run_task import discover_tasks
        
        for i in range(1, 20):
            name = f"M{i:03d}_test"
            task_dir = tmp_path / name
            task_dir.mkdir()
            (task_dir / "task.yaml").write_text(f"task_id: {name}\n")
        
        results = discover_tasks(str(tmp_path), prefix="M", range_str="1-10")
        assert len(results) == 10

    def test_range_c_tasks_with_prefix(self, tmp_path):
        """Filter C tasks by range with --prefix."""
        from ce_runner.run_task import discover_tasks
        
        for i in range(1, 15):
            name = f"C{i:02d}_test"
            task_dir = tmp_path / name
            task_dir.mkdir()
            (task_dir / "task.yaml").write_text(f"task_id: {name}\n")
        
        results = discover_tasks(str(tmp_path), prefix="C", range_str="1-5")
        assert len(results) == 5

    def test_range_invalid_format(self, tmp_path):
        """Invalid range format should exit."""
        from ce_runner.run_task import discover_tasks
        
        with pytest.raises(SystemExit):
            discover_tasks(str(tmp_path), range_str="invalid")


class TestDiscoverTasksCombined:
    """Test combined filters."""

    def test_prefix_and_range(self, tmp_path):
        """Combine --prefix and --range."""
        from ce_runner.run_task import discover_tasks
        
        for i in range(1, 20):
            for prefix in ["T", "M", "C"]:
                name = f"{prefix}{i:03d}_test"
                task_dir = tmp_path / name
                task_dir.mkdir()
                (task_dir / "task.yaml").write_text(f"task_id: {name}\n")
        
        results = discover_tasks(str(tmp_path), prefix="M", range_str="5-10")
        assert len(results) == 6
        assert all(Path(r).name.startswith("M") for r in results)

    def test_filter_and_tag(self, tmp_path):
        """Combine --filter and --tag."""
        from ce_runner.run_task import discover_tasks
        
        tasks = [
            ("T001zh_email", ["general"]),
            ("T002zh_sms", ["general"]),
            ("M001zh_clock", ["multimodal"])
        ]
        for name, tags in tasks:
            task_dir = tmp_path / name
            task_dir.mkdir()
            (task_dir / "task.yaml").write_text(f"task_id: {name}\ntags:\n{chr(10).join('  - ' + t for t in tags)}\n")
        
        results = discover_tasks(str(tmp_path), filter_str="zh", tag="general")
        assert len(results) == 2


class TestDiscoverTasksEdgeCases:
    """Test edge cases."""

    def test_empty_directory(self, tmp_path):
        """Empty directory returns empty list."""
        from ce_runner.run_task import discover_tasks
        
        results = discover_tasks(str(tmp_path))
        assert results == []

    def test_nonexistent_directory(self):
        """Nonexistent directory should exit."""
        from ce_runner.run_task import discover_tasks
        
        with pytest.raises(SystemExit):
            discover_tasks("/nonexistent/path")

    def test_no_task_yaml(self, tmp_path):
        """Directories without task.yaml are ignored."""
        from ce_runner.run_task import discover_tasks
        
        (tmp_path / "T001_test").mkdir()
        # No task.yaml
        
        results = discover_tasks(str(tmp_path))
        assert results == []
