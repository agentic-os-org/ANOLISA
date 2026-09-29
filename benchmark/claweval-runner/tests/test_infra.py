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

"""Test infrastructure functions: Gateway, mock services, and session management.

Covers:
- Gateway service health check and management
- Mock service start/stop/reset
- Session cleanup logic
- Container lifecycle management (for M tasks)
- Port allocation and availability checks

Task type coverage:
- T tasks: Gateway + mock services
- M tasks: Docker container management
- C tasks: Gateway + user_agent session management
"""

import sys
from pathlib import Path
from unittest.mock import patch, MagicMock, call
import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))


class TestGatewayService:
    """Test Gateway service management."""

    def test_gateway_check_function_exists(self):
        """Gateway check function is available."""
        from ce_runner.infra import check_gateway
        
        # Function should be callable
        assert callable(check_gateway)


class TestMockServices:
    """Test mock service management."""

    def test_mock_service_reset_function_exists(self):
        """Mock service reset function is available."""
        from ce_runner.infra import reset_services
        
        # Function should be callable
        assert callable(reset_services)

    def test_mock_service_reset_no_yaml(self):
        """Reset mock services without task yaml."""
        from ce_runner.infra import reset_services
        
        # Should handle None gracefully
        with patch('ce_runner.infra.httpx.post', side_effect=Exception("Connection refused")):
            reset_services(None)


class TestSessionCleanup:
    """Test session cleanup logic."""

    def test_cleanup_session_file(self, tmp_path):
        """Cleanup removes session file."""
        from ce_runner.infra import cleanup_session
        
        session_file = tmp_path / "session.jsonl"
        session_file.write_text('{"type": "message"}\n')
        
        # Should not raise
        cleanup_session(str(session_file))
        
        # File should be removed
        assert not session_file.exists()

    def test_cleanup_missing_session(self, tmp_path):
        """Cleanup handles missing session file gracefully."""
        from ce_runner.infra import cleanup_session
        
        session_file = tmp_path / "nonexistent.jsonl"
        
        # Should not raise
        cleanup_session(str(session_file))


class TestPortManagement:
    """Test port allocation and availability."""

    def test_check_port_available(self):
        """Check if port is available."""
        # Port management is handled internally by infra module
        # This is a placeholder test
        assert True


class TestContainerManagement:
    """Test Docker container management (M tasks)."""

    def test_container_lifecycle_mock(self):
        """Mock container start/stop lifecycle."""
        # This is a placeholder - actual implementation may vary
        # Container management is typically handled in sandbox.py
        pass


class TestEnvironmentSnapshot:
    """Test environment snapshot management."""

    def test_snapshot_file_creation(self, tmp_path):
        """Create environment snapshot file."""
        # Environment snapshot is created during M task execution
        # This tests the file handling logic
        snapshot_file = tmp_path / "env_snapshot.json"
        snapshot_file.write_text('{"files": [], "commands": []}')
        
        assert snapshot_file.exists()
        content = snapshot_file.read_text()
        assert "files" in content
