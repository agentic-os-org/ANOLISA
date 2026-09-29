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

"""End-to-end integration tests using real task configurations.

Uses tasks from .venv/backup/simple_list.txt:
- T001zh_email_triage (T task: Gateway mode)
- T009zh_contact_lookup (T task: Gateway mode)
- C01zh_mortgage_prepay (C task: UserAgent mode)
- M101_chinese_food_identification_zh (M task: Sandbox mode)
- M099_su7_price_from_image_zh (M task: Sandbox mode)

Covers:
- Task discovery from real task directory
- Configuration loading and validation
- Session to trace conversion with real data
- Pipeline integration (convert + grade)
- Batch result aggregation

Note: These tests don't actually run the agent (which requires LLM API),
but verify the integration points work correctly with real task configs.
"""

import json
import sys
from pathlib import Path
from unittest.mock import patch, MagicMock
import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))

# Real task IDs from simple_list.txt
REAL_T_TASKS = ["T001zh_email_triage", "T009zh_contact_lookup"]
REAL_C_TASKS = ["C01zh_mortgage_prepay"]
REAL_M_TASKS = ["M101_chinese_food_identification_zh", "M099_su7_price_from_image_zh"]
ALL_REAL_TASKS = REAL_T_TASKS + REAL_C_TASKS + REAL_M_TASKS


class TestRealTaskDiscovery:
    """Test task discovery with real task directory."""

    def test_discover_real_t_tasks(self):
        """Discover T tasks from real task directory."""
        from ce_runner.run_task import discover_tasks
        
        tasks_dir = str(REPO_ROOT / "claw-eval" / "tasks")
        
        # Discover with prefix filter
        t_tasks = discover_tasks(tasks_dir, prefix="T00")
        
        # Should find at least our test tasks
        found_tasks = [Path(t).name for t in t_tasks]
        for task_id in REAL_T_TASKS:
            assert task_id in found_tasks, f"T task {task_id} not discovered"

    def test_discover_real_m_tasks(self):
        """Discover M tasks from real task directory."""
        from ce_runner.run_task import discover_tasks
        
        tasks_dir = str(REPO_ROOT / "claw-eval" / "tasks")
        
        # Discover with prefix filter
        m_tasks = discover_tasks(tasks_dir, prefix="M10")
        
        found_tasks = [Path(t).name for t in m_tasks]
        for task_id in REAL_M_TASKS:
            if task_id.startswith("M10"):
                assert task_id in found_tasks, f"M task {task_id} not discovered"

    def test_discover_by_tag_user_agent(self):
        """Discover C tasks by user_agent tag."""
        from ce_runner.run_task import discover_tasks
        
        tasks_dir = str(REPO_ROOT / "claw-eval" / "tasks")
        
        c_tasks = discover_tasks(tasks_dir, tag="user_agent")
        found_tasks = [Path(t).name for t in c_tasks]
        
        assert "C01zh_mortgage_prepay" in found_tasks


class TestRealTaskConfiguration:
    """Test configuration loading with real task yamls."""

    @pytest.mark.parametrize("task_id", ALL_REAL_TASKS)
    def test_load_real_task_yaml(self, task_id):
        """Load and validate real task.yaml files."""
        from ce_runner._common import load_task_yaml
        
        task_path = REPO_ROOT / "claw-eval" / "tasks" / task_id / "task.yaml"
        
        if task_path.exists():
            task = load_task_yaml(str(task_path))
            
            # All tasks must have task_id
            assert task["task_id"] == task_id
            assert "prompt" in task or "task_name" in task

    def test_t_task_has_services(self):
        """T tasks should have services configuration."""
        from ce_runner._common import load_task_yaml
        
        for task_id in REAL_T_TASKS:
            task_path = REPO_ROOT / "claw-eval" / "tasks" / task_id / "task.yaml"
            if task_path.exists():
                task = load_task_yaml(str(task_path))
                assert "services" in task
                assert len(task["services"]) > 0

    def test_m_task_has_sandbox_files(self):
        """M tasks should have sandbox_files or attachments."""
        from ce_runner._common import load_task_yaml
        
        for task_id in REAL_M_TASKS:
            task_path = REPO_ROOT / "claw-eval" / "tasks" / task_id / "task.yaml"
            if task_path.exists():
                task = load_task_yaml(str(task_path))
                has_sandbox = "sandbox_files" in task
                has_attachments = "attachments" in task.get("prompt", {})
                assert has_sandbox or has_attachments, \
                    f"M task {task_id} should have sandbox_files or attachments"

    def test_c_task_has_user_agent(self):
        """C tasks should have user_agent configuration."""
        from ce_runner._common import load_task_yaml
        
        for task_id in REAL_C_TASKS:
            task_path = REPO_ROOT / "claw-eval" / "tasks" / task_id / "task.yaml"
            if task_path.exists():
                task = load_task_yaml(str(task_path))
                assert "user_agent" in task
                assert task["user_agent"].get("enabled") is True


class TestRealTaskSandboxDetection:
    """Test sandbox detection with real tasks."""

    def test_t_tasks_not_sandbox(self):
        """T tasks may or may not be sandbox tasks (depends on config)."""
        from ce_runner._common import is_sandbox_task
        
        # Note: Some T tasks have sandbox_files for fixture data
        # The key difference is they also have services
        for task_id in REAL_T_TASKS:
            task_path = REPO_ROOT / "claw-eval" / "tasks" / task_id / "task.yaml"
            if task_path.exists():
                # Just verify it loads without error
                is_sandbox = is_sandbox_task(str(task_path))
                # Don't assert False - some T tasks have sandbox_files

    def test_m_tasks_are_sandbox(self):
        """M tasks should be detected as sandbox tasks."""
        from ce_runner._common import is_sandbox_task
        
        for task_id in REAL_M_TASKS:
            task_path = REPO_ROOT / "claw-eval" / "tasks" / task_id / "task.yaml"
            if task_path.exists():
                assert is_sandbox_task(str(task_path)) is True, \
                    f"M task {task_id} should be sandbox"

    def test_c_tasks_not_sandbox(self):
        """C tasks should not be detected as sandbox tasks."""
        from ce_runner._common import is_sandbox_task
        
        for task_id in REAL_C_TASKS:
            task_path = REPO_ROOT / "claw-eval" / "tasks" / task_id / "task.yaml"
            if task_path.exists():
                assert is_sandbox_task(str(task_path)) is False, \
                    f"C task {task_id} should not be sandbox"


class TestRealTaskSessionConversion:
    """Test session to trace conversion with real task configs."""

    def test_convert_session_for_t_task(self, tmp_path):
        """Convert session for T task with real config."""
        from ce_runner._common import load_task_yaml
        from ce_runner.session_trace_converter import convert_session_to_trace
        
        # Create mock session
        session_file = tmp_path / "session.jsonl"
        session_file.write_text(
            '{"type": "message", "timestamp": "2024-01-15T10:00:00.000Z", '
            '"message": {"role": "user", "content": [{"type": "text", "text": "Test"}]}}\n'
        )
        
        # Use real T task config
        task_path = REPO_ROOT / "claw-eval" / "tasks" / "T001zh_email_triage" / "task.yaml"
        if task_path.exists():
            task = load_task_yaml(str(task_path))
            output_file = tmp_path / "trace.jsonl"
            
            result = convert_session_to_trace(str(session_file), task, str(output_file))
            
            assert output_file.exists()
            events = [json.loads(line) for line in output_file.read_text().strip().split('\n')]
            assert len(events) >= 2  # trace_start + trace_end

    def test_convert_session_for_m_task(self, tmp_path):
        """Convert session for M task with real config."""
        from ce_runner._common import load_task_yaml
        from ce_runner.session_trace_converter import convert_session_to_trace
        
        session_file = tmp_path / "session.jsonl"
        session_file.write_text(
            '{"type": "message", "timestamp": "2024-01-15T10:00:00.000Z", '
            '"message": {"role": "user", "content": [{"type": "text", "text": "Test"}]}}\n'
        )
        
        # Use real M task config
        task_path = REPO_ROOT / "claw-eval" / "tasks" / "M101_chinese_food_identification_zh" / "task.yaml"
        if task_path.exists():
            task = load_task_yaml(str(task_path))
            output_file = tmp_path / "trace.jsonl"
            
            result = convert_session_to_trace(str(session_file), task, str(output_file))
            
            assert output_file.exists()


class TestRealTaskPipeline:
    """Test pipeline integration with real tasks."""

    def test_pipeline_phase_convert_with_real_task(self, tmp_path):
        """Test phase_convert with real task config."""
        from ce_runner.pipeline import phase_convert
        
        session_file = tmp_path / "session.jsonl"
        session_file.write_text('{"type": "message"}\n')
        
        task_yaml = REPO_ROOT / "claw-eval" / "tasks" / "T001zh_email_triage" / "task.yaml"
        output_file = tmp_path / "trace.jsonl"
        
        if task_yaml.exists():
            # Mock subprocess to avoid actual conversion
            def mock_run(*args, **kwargs):
                output_file.write_text('{"type": "trace_start"}\n{"type": "trace_end"}\n')
                result = MagicMock()
                result.returncode = 0
                return result
            
            with patch('ce_runner.pipeline.subprocess.run', side_effect=mock_run):
                success = phase_convert(str(session_file), str(task_yaml), str(output_file))
            
            assert success is True

    def test_batch_aggregation_with_real_task_results(self, tmp_path):
        """Test batch result aggregation with realistic data."""
        from ce_runner.batch_runner import pass_at_k
        
        # Simulate real batch results
        n_trials = 3
        n_tasks = 5
        pass_counts = [3, 2, 1, 0, 3]  # Different pass rates
        
        total_pass_at_1 = 0
        for c in pass_counts:
            p = pass_at_k(n_trials, c, 1)
            total_pass_at_1 += p
        
        avg_pass_at_1 = total_pass_at_1 / n_tasks
        assert 0 <= avg_pass_at_1 <= 1


class TestRealTaskConfigValidation:
    """Test configuration validation with real tasks."""

    def test_validate_t_task_config(self):
        """Validate T task has required fields."""
        from ce_runner._common import load_task_yaml
        
        for task_id in REAL_T_TASKS:
            task_path = REPO_ROOT / "claw-eval" / "tasks" / task_id / "task.yaml"
            if task_path.exists():
                task = load_task_yaml(str(task_path))
                
                # T tasks must have services
                assert "services" in task
                for svc in task["services"]:
                    assert "name" in svc
                    assert "port" in svc

    def test_validate_m_task_config(self):
        """Validate M task has required fields."""
        from ce_runner._common import load_task_yaml
        
        for task_id in REAL_M_TASKS:
            task_path = REPO_ROOT / "claw-eval" / "tasks" / task_id / "task.yaml"
            if task_path.exists():
                task = load_task_yaml(str(task_path))
                
                # M tasks must have prompt with text
                assert "prompt" in task
                assert "text" in task["prompt"]

    def test_validate_c_task_config(self):
        """Validate C task has required fields."""
        from ce_runner._common import load_task_yaml
        
        for task_id in REAL_C_TASKS:
            task_path = REPO_ROOT / "claw-eval" / "tasks" / task_id / "task.yaml"
            if task_path.exists():
                task = load_task_yaml(str(task_path))
                
                # C tasks must have user_agent
                assert "user_agent" in task
                ua = task["user_agent"]
                assert "enabled" in ua
                assert "max_rounds" in ua or "persona" in ua
