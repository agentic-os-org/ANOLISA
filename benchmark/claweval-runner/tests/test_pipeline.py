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

"""Test pipeline functions: session archiving, conversion, and grading.

Covers:
- archive_session_alongside_trace (success/failure scenarios)
- phase_convert subprocess invocation
- phase_grade subprocess invocation and err_log cleanup
- convert_and_grade full pipeline
- Error handling (missing files/failed subprocesses)

Task type coverage:
- All types: session archival before cleanup
- T/M/C tasks: trace conversion and grading pipeline
"""

import json
import sys
from pathlib import Path
from unittest.mock import patch, MagicMock, mock_open
import pytest
import os

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))


class TestArchiveSession:
    """Test session archival alongside trace."""

    def test_archive_success(self, tmp_path):
        """Successfully archive session file."""
        from ce_runner.pipeline import archive_session_alongside_trace
        
        # Create session file
        session_file = tmp_path / "session.jsonl"
        session_file.write_text('{"type": "message"}\n')
        
        trace_dir = tmp_path / "traces"
        trace_dir.mkdir()
        trace_file = str(trace_dir / "trace_001.jsonl")
        
        result = archive_session_alongside_trace(str(session_file), trace_file)
        
        assert result != ""
        archived_path = Path(result)
        assert archived_path.exists()
        assert "sessions" in str(archived_path)
        assert "trace_001.session.jsonl" in str(archived_path)

    def test_archive_missing_session(self):
        """Return empty string when session doesn't exist."""
        from ce_runner.pipeline import archive_session_alongside_trace
        
        result = archive_session_alongside_trace("/nonexistent/session.jsonl", "/tmp/trace.jsonl")
        assert result == ""

    def test_archive_creates_sessions_dir(self, tmp_path):
        """Create sessions directory if it doesn't exist."""
        from ce_runner.pipeline import archive_session_alongside_trace
        
        session_file = tmp_path / "session.jsonl"
        session_file.write_text('{"type": "message"}\n')
        
        trace_dir = tmp_path / "traces"
        trace_dir.mkdir()
        trace_file = str(trace_dir / "trace_001.jsonl")
        
        archive_session_alongside_trace(str(session_file), trace_file)
        
        sessions_dir = trace_dir / "sessions"
        assert sessions_dir.exists()
        assert sessions_dir.is_dir()


class TestPhaseConvert:
    """Test phase_convert subprocess invocation."""

    def test_convert_success(self, tmp_path):
        """phase_convert returns True on success."""
        from ce_runner.pipeline import phase_convert
        
        session_file = tmp_path / "session.jsonl"
        session_file.write_text('{"type": "message"}\n')
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices: []\ntools: []\n")
        
        output_file = tmp_path / "output.jsonl"
        
        # Mock subprocess to create output file
        def mock_run(*args, **kwargs):
            output_file.write_text("{}\n")
            result = MagicMock()
            result.returncode = 0
            return result
        
        with patch('ce_runner.pipeline.subprocess.run', side_effect=mock_run):
            result = phase_convert(str(session_file), str(task_yaml), str(output_file))
        
        assert result is True

    def test_convert_failure_returncode(self, tmp_path):
        """phase_convert returns False on non-zero returncode."""
        from ce_runner.pipeline import phase_convert
        
        session_file = tmp_path / "session.jsonl"
        session_file.write_text('{"type": "message"}\n')
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices: []\ntools: []\n")
        
        output_file = tmp_path / "output.jsonl"
        
        mock_result = MagicMock()
        mock_result.returncode = 1
        
        with patch('ce_runner.pipeline.subprocess.run', return_value=mock_result):
            result = phase_convert(str(session_file), str(task_yaml), str(output_file))
        
        assert result is False


class TestPhaseGrade:
    """Test phase_grade subprocess invocation."""

    def test_grade_success(self, tmp_path):
        """phase_grade runs grading subprocess."""
        from ce_runner.pipeline import phase_grade
        
        trace_file = tmp_path / "trace.jsonl"
        trace_file.write_text('{"type": "trace_start"}\n')
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices: []\ntools: []\n")
        
        judge_config = {
            "model": "test-model",
            "base_url": "http://test.com",
            "api_key": "test-key"
        }
        
        mock_result = MagicMock()
        mock_result.returncode = 0
        
        with patch('ce_runner.pipeline.subprocess.run', return_value=mock_result):
            with patch('ce_runner.pipeline.os.path.getsize', return_value=0):
                with patch('ce_runner.pipeline.os.remove'):
                    result = phase_grade(str(trace_file), str(task_yaml), judge_config)
        
        assert isinstance(result, dict)

    def test_grade_with_env_snapshot(self, tmp_path):
        """phase_grade includes env_snapshot when provided."""
        from ce_runner.pipeline import phase_grade
        
        trace_file = tmp_path / "trace.jsonl"
        trace_file.write_text('{"type": "trace_start"}\n')
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: M001\nservices: []\ntools: []\n")
        
        judge_config = {
            "model": "test-model",
            "base_url": "http://test.com",
            "api_key": "test-key"
        }
        
        snapshot_path = tmp_path / "snapshot.json"
        snapshot_path.write_text("{}")
        
        def mock_run(cmd, **kwargs):
            assert "--env-snapshot" in cmd
            result = MagicMock()
            result.returncode = 0
            return result
        
        with patch('ce_runner.pipeline.subprocess.run', side_effect=mock_run):
            with patch('ce_runner.pipeline.os.path.getsize', return_value=0):
                with patch('ce_runner.pipeline.os.remove'):
                    phase_grade(str(trace_file), str(task_yaml), judge_config, 
                               str(snapshot_path))


class TestErrLogCleanup:
    """Test grader stderr log cleanup logic."""

    def test_cleanup_empty_log(self, tmp_path):
        """Remove err_log when empty."""
        from ce_runner.pipeline import phase_grade
        
        trace_file = tmp_path / "trace.jsonl"
        trace_file.write_text('{"type": "trace_start"}\n')
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices: []\ntools: []\n")
        
        judge_config = {
            "model": "test-model",
            "base_url": "http://test.com",
            "api_key": "test-key"
        }
        
        err_log = tmp_path / f"grader_{trace_file.stem}.err.log"
        err_log.write_text("")
        
        mock_result = MagicMock()
        mock_result.returncode = 0
        
        def mock_getsize(path):
            return 0
        
        removed = []
        def mock_remove(path):
            removed.append(path)
        
        with patch('ce_runner.pipeline.subprocess.run', return_value=mock_result):
            with patch('ce_runner.pipeline.os.path.getsize', side_effect=mock_getsize):
                with patch('ce_runner.pipeline.os.remove', side_effect=mock_remove):
                    phase_grade(str(trace_file), str(task_yaml), judge_config)
        
        assert len(removed) > 0

    def test_keep_log_on_failure(self, tmp_path):
        """Keep err_log when grader fails (rc != 0)."""
        from ce_runner.pipeline import phase_grade
        
        trace_file = tmp_path / "trace.jsonl"
        trace_file.write_text('{"type": "trace_start"}\n')
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices: []\ntools: []\n")
        
        judge_config = {
            "model": "test-model",
            "base_url": "http://test.com",
            "api_key": "test-key"
        }
        
        mock_result = MagicMock()
        mock_result.returncode = 1
        
        removed = []
        def mock_remove(path):
            removed.append(path)
        
        with patch('ce_runner.pipeline.subprocess.run', return_value=mock_result):
            with patch('ce_runner.pipeline.os.path.getsize', return_value=100):
                with patch('ce_runner.pipeline.os.remove', side_effect=mock_remove):
                    with patch('ce_runner.pipeline.open', mock_open(read_data=b"error")):
                        phase_grade(str(trace_file), str(task_yaml), judge_config)
        
        # Should not remove on failure
        assert len(removed) == 0
