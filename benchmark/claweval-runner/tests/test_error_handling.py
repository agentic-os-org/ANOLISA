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

"""Test error handling and recovery mechanisms.

Covers:
- Graceful degradation on missing files
- Error propagation in batch mode
- Subprocess failure handling
- Configuration error detection
- Network/service failure recovery

Task type coverage:
- T tasks: Service startup failures
- M tasks: Container execution errors
- C tasks: UserAgent dialogue failures
- All types: Config validation errors
"""

import json
import sys
from pathlib import Path
from unittest.mock import patch, MagicMock
import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))


class TestMissingFileHandling:
    """Test graceful handling of missing files."""

    def test_missing_session_file(self, tmp_path):
        """Handle missing session file gracefully."""
        from ce_runner.pipeline import archive_session_alongside_trace

        trace_file = str(tmp_path / "trace.jsonl")
        result = archive_session_alongside_trace("/nonexistent/session.jsonl", trace_file)

        # Should return empty string, not raise
        assert result == ""

    def test_missing_task_yaml(self, tmp_path):
        """Handle missing task.yaml gracefully."""
        from ce_runner._common import load_task_yaml

        with pytest.raises(FileNotFoundError):
            load_task_yaml("/nonexistent/task.yaml")

    def test_missing_trace_directory(self, tmp_path):
        """Create trace directory if it doesn't exist."""
        from ce_runner.pipeline import archive_session_alongside_trace

        session_file = tmp_path / "session.jsonl"
        session_file.write_text('{"type": "message"}\n')

        trace_dir = tmp_path / "new_traces"
        trace_file = str(trace_dir / "trace.jsonl")

        result = archive_session_alongside_trace(str(session_file), trace_file)

        # Should create directory and succeed
        assert result != ""
        assert trace_dir.exists()


class TestSubprocessFailure:
    """Test subprocess failure handling."""

    def test_phase_convert_failure(self, tmp_path):
        """Handle phase_convert subprocess failure."""
        from ce_runner.pipeline import phase_convert

        session_file = tmp_path / "session.jsonl"
        session_file.write_text('{"type": "message"}\n')

        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices: []\n")

        output_file = tmp_path / "trace.jsonl"

        # Mock subprocess failure
        mock_result = MagicMock()
        mock_result.returncode = 1

        with patch('ce_runner.pipeline.subprocess.run', return_value=mock_result):
            result = phase_convert(str(session_file), str(task_yaml), str(output_file))

        # Should return False, not raise
        assert result is False

    def test_phase_grade_failure(self, tmp_path):
        """Handle phase_grade subprocess failure."""
        from ce_runner.pipeline import phase_grade

        trace_file = tmp_path / "trace.jsonl"
        trace_file.write_text('{"type": "trace_start"}\n')

        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\n")

        judge_config = {
            "model": "test",
            "base_url": "http://test",
            "api_key": "key"
        }

        # Mock subprocess failure
        mock_result = MagicMock()
        mock_result.returncode = 1

        with patch('ce_runner.pipeline.subprocess.run', return_value=mock_result):
            with patch('ce_runner.pipeline.os.path.getsize', return_value=100):
                with patch('ce_runner.pipeline.open', MagicMock()):
                    result = phase_grade(str(trace_file), str(task_yaml), judge_config)

        # Should return dict (may be empty), not raise
        assert isinstance(result, dict)


class TestConfigurationErrors:
    """Test configuration error detection."""

    def test_invalid_task_yaml_syntax(self, tmp_path):
        """Detect invalid YAML syntax."""
        from ce_runner._common import load_task_yaml

        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("invalid: yaml: syntax:\n  - broken")

        with pytest.raises(Exception):
            load_task_yaml(str(task_yaml))

    def test_missing_required_fields(self, tmp_path):
        """Detect missing required fields in task.yaml."""
        from ce_runner._common import load_task_yaml

        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\n")  # Missing services/prompt

        task = load_task_yaml(str(task_yaml))

        # Should load but may be incomplete
        assert "task_id" in task


class TestBatchErrorPropagation:
    """Test error propagation in batch mode."""

    def test_pass_at_k_with_zero_trials(self):
        """Handle edge case of zero trials."""
        from ce_runner.batch_runner import pass_at_k

        # Should handle n=0 gracefully
        result = pass_at_k(0, 0, 1)
        assert isinstance(result, float)

    def test_pass_at_k_with_all_failures(self):
        """Handle case where all trials fail."""
        from ce_runner.batch_runner import pass_at_k

        # n=3 trials, c=0 passed, k=1
        result = pass_at_k(3, 0, 1)
        assert result == 0.0

    def test_pass_at_k_with_partial_success(self):
        """Handle partial success correctly."""
        from ce_runner.batch_runner import pass_at_k

        # n=3 trials, c=2 passed, k=1
        result = pass_at_k(3, 2, 1)
        assert 0 < result < 1


class TestNetworkFailureRecovery:
    """Test network/service failure recovery."""

    def test_gateway_health_check_timeout(self, tmp_path):
        """Handle gateway health check timeout."""
        from ce_runner.infra import check_gateway

        # check_gateway expects JSON config, not YAML
        config_file = tmp_path / "config.json"
        config_file.write_text('{"gateway": {"port": 3000}}')

        # Mock timeout
        with patch('ce_runner.infra.httpx.get', side_effect=TimeoutError("Connection timed out")):
            # Should return 0 (failure), not raise
            result = check_gateway(str(config_file))
            assert result == 0

    def test_mock_service_reset_failure(self, tmp_path):
        """Handle mock service reset failure."""
        from ce_runner.infra import reset_services

        # Mock connection refused
        with patch('ce_runner.infra.httpx.post',
                   side_effect=ConnectionRefusedError("Connection refused")):
            # Should not raise, just log warning
            reset_services(None)


class TestSessionConversionErrors:
    """Test session conversion error handling."""

    def test_convert_empty_session(self, tmp_path):
        """Handle empty session file."""
        from ce_runner._common import load_task_yaml
        from ce_runner.session_trace_converter import convert_session_to_trace

        session_file = tmp_path / "session.jsonl"
        session_file.write_text("")

        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices: []\n")
        task = load_task_yaml(str(task_yaml))

        output_file = tmp_path / "trace.jsonl"

        # Should create minimal trace, not raise
        result = convert_session_to_trace(str(session_file), task, str(output_file))
        assert output_file.exists()

    def test_convert_invalid_json_lines(self, tmp_path):
        """Handle session with invalid JSON lines."""
        from ce_runner._common import load_task_yaml
        from ce_runner.session_trace_converter import convert_session_to_trace

        session_file = tmp_path / "session.jsonl"
        session_file.write_text(
            "invalid json\n"
            '{"type": "message", "timestamp": "2024-01-15T10:00:00.000Z", '
            '"message": {"role": "user", "content": []}}\n'
            "more invalid text\n"
        )

        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices: []\n")
        task = load_task_yaml(str(task_yaml))

        output_file = tmp_path / "trace.jsonl"

        # Should skip invalid lines, not raise
        result = convert_session_to_trace(str(session_file), task, str(output_file))
        assert output_file.exists()

    def test_convert_missing_timestamp(self, tmp_path):
        """Handle messages without timestamp."""
        from ce_runner._common import load_task_yaml
        from ce_runner.session_trace_converter import convert_session_to_trace

        session_file = tmp_path / "session.jsonl"
        session_file.write_text(
            '{"type": "message", "message": {"role": "user", "content": []}}\n'
        )

        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices: []\n")
        task = load_task_yaml(str(task_yaml))

        output_file = tmp_path / "trace.jsonl"

        # Should handle missing timestamp, not raise
        result = convert_session_to_trace(str(session_file), task, str(output_file))
        assert output_file.exists()


class TestCleanupOnFailure:
    """Test cleanup behavior on failure."""

    def test_cleanup_missing_session_file(self, tmp_path):
        """Cleanup should handle already-deleted session."""
        from ce_runner.infra import cleanup_session

        session_file = tmp_path / "session.jsonl"
        # Don't create the file

        # Should not raise
        cleanup_session(str(session_file))

    def test_pipeline_err_log_cleanup_on_success(self, tmp_path):
        """Remove err_log when grader succeeds with empty stderr."""
        from ce_runner.pipeline import phase_grade

        trace_file = tmp_path / "trace.jsonl"
        trace_file.write_text('{"type": "trace_start"}\n')

        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\n")

        judge_config = {
            "model": "test",
            "base_url": "http://test",
            "api_key": "key"
        }

        err_log = tmp_path / f"grader_{trace_file.stem}.err.log"
        err_log.write_text("")

        mock_result = MagicMock()
        mock_result.returncode = 0

        removed_files = []
        def mock_remove(path):
            removed_files.append(path)

        with patch('ce_runner.pipeline.subprocess.run', return_value=mock_result):
            with patch('ce_runner.pipeline.os.path.getsize', return_value=0):
                with patch('ce_runner.pipeline.os.remove', side_effect=mock_remove):
                    phase_grade(str(trace_file), str(task_yaml), judge_config)

        # Should remove empty err_log
        assert len(removed_files) > 0
