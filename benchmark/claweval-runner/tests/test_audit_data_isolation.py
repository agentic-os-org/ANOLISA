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

"""Tests for Bug 1 fix: cross-trial audit data contamination (P0).

Verifies:
- convert_session_to_trace uses preloaded_audit_data when provided
- convert_session_to_trace skips live fetch when preloaded data is given
- phase_convert passes audit_data_path to subprocess CLI
- CLI --audit-data flag loads and uses pre-saved audit JSON
"""

import json
import sys
from pathlib import Path
from unittest.mock import patch, MagicMock
import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))


class TestPreloadedAuditData:
    """Test that preloaded audit data bypasses live fetch."""

    def test_preloaded_audit_data_used(self, tmp_path):
        """When preloaded_audit_data is provided, fetch_audit_data is NOT called."""
        from ce_runner.session_trace_converter import convert_session_to_trace

        session_file = tmp_path / "session.jsonl"
        session_file.write_text(
            '{"type": "message", "timestamp": "2024-01-15T10:00:00.000Z", '
            '"message": {"role": "user", "content": []}}\n'
        )

        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text(
            "task_id: T011zh\n"
            "services:\n"
            "  - name: finance\n"
            "    reset_endpoint: http://localhost:9100/finance/reset\n"
            "tools: []\n"
        )

        output_file = tmp_path / "output.jsonl"

        # Provide preloaded audit data
        preloaded = {
            "finance": {
                "submitted_reports": [
                    {"report_id": "RPT-001", "amount": 1500.0, "timestamp": "2024-01-15T10:05:00Z"}
                ]
            }
        }

        from ce_runner._common import load_task_yaml
        task = load_task_yaml(str(task_yaml))

        # Patch fetch_audit_data to verify it is NOT called
        with patch("ce_runner.session_trace_converter.fetch_audit_data") as mock_fetch:
            mock_fetch.return_value = {"finance": {"submitted_reports": []}}  # empty = wrong data

            result = convert_session_to_trace(
                str(session_file), task, str(output_file),
                preloaded_audit_data=preloaded,
            )

            # fetch_audit_data should NOT have been called
            mock_fetch.assert_not_called()

        # Verify output contains the preloaded audit data
        events = [json.loads(line) for line in output_file.read_text().strip().split("\n")]
        audit_events = [e for e in events if e.get("type") == "audit_snapshot"]
        assert len(audit_events) == 1
        assert audit_events[0]["service_name"] == "finance"
        # Must contain the preloaded data (RPT-001), NOT the mock's empty data
        reports = audit_events[0]["audit_data"]["submitted_reports"]
        assert len(reports) == 1
        assert reports[0]["report_id"] == "RPT-001"

    def test_no_preloaded_falls_back_to_fetch(self, tmp_path):
        """When preloaded_audit_data is None, fetch_audit_data IS called."""
        from ce_runner.session_trace_converter import convert_session_to_trace

        session_file = tmp_path / "session.jsonl"
        session_file.write_text(
            '{"type": "message", "timestamp": "2024-01-15T10:00:00.000Z", '
            '"message": {"role": "user", "content": []}}\n'
        )

        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices: []\ntools: []\n")

        output_file = tmp_path / "output.jsonl"

        from ce_runner._common import load_task_yaml
        task = load_task_yaml(str(task_yaml))

        with patch("ce_runner.session_trace_converter.fetch_audit_data") as mock_fetch:
            mock_fetch.return_value = {}
            convert_session_to_trace(
                str(session_file), task, str(output_file),
                preloaded_audit_data=None,
            )
            mock_fetch.assert_called_once()

    def test_different_trials_get_different_audit_data(self, tmp_path):
        """Simulate two trials with different preloaded audit data — no contamination."""
        from ce_runner.session_trace_converter import convert_session_to_trace

        task_yaml_content = (
            "task_id: T011zh\n"
            "services:\n"
            "  - name: finance\n"
            "    reset_endpoint: http://localhost:9100/finance/reset\n"
            "tools: []\n"
        )
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text(task_yaml_content)

        from ce_runner._common import load_task_yaml
        task = load_task_yaml(str(task_yaml))

        # Trial 1: agent submitted a report
        trial1_audit = {
            "finance": {
                "submitted_reports": [
                    {"report_id": "RPT-001", "amount": 1500.0, "timestamp": "2024-01-15T10:05:00Z"}
                ]
            }
        }

        # Trial 2: agent did NOT submit anything
        trial2_audit = {
            "finance": {
                "submitted_reports": []
            }
        }

        for trial_num, audit_data in [(1, trial1_audit), (2, trial2_audit)]:
            session_file = tmp_path / f"session_t{trial_num}.jsonl"
            session_file.write_text(
                '{"type": "message", "timestamp": "2024-01-15T10:00:00.000Z", '
                '"message": {"role": "user", "content": []}}\n'
            )
            output_file = tmp_path / f"output_t{trial_num}.jsonl"

            convert_session_to_trace(
                str(session_file), task, str(output_file),
                preloaded_audit_data=audit_data,
            )

        # Verify trial 1 has the report
        t1_events = [json.loads(l) for l in (tmp_path / "output_t1.jsonl").read_text().strip().split("\n")]
        t1_audit = [e for e in t1_events if e["type"] == "audit_snapshot"][0]
        assert len(t1_audit["audit_data"]["submitted_reports"]) == 1

        # Verify trial 2 has NO report (no contamination from trial 1)
        t2_events = [json.loads(l) for l in (tmp_path / "output_t2.jsonl").read_text().strip().split("\n")]
        t2_audit = [e for e in t2_events if e["type"] == "audit_snapshot"][0]
        assert len(t2_audit["audit_data"]["submitted_reports"]) == 0


class TestPhaseConvertAuditDataPath:
    """Test that phase_convert passes audit_data_path to subprocess."""

    def test_audit_data_path_in_command(self, tmp_path):
        """phase_convert includes --audit-data in subprocess command."""
        from ce_runner.pipeline import phase_convert

        session_file = tmp_path / "session.jsonl"
        session_file.write_text('{"type": "message"}\n')

        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices: []\ntools: []\n")

        output_file = tmp_path / "output.jsonl"
        audit_file = tmp_path / "audit.json"
        audit_file.write_text("{}")

        def mock_run(cmd, **kwargs):
            assert "--audit-data" in cmd
            idx = cmd.index("--audit-data")
            assert cmd[idx + 1] == str(audit_file)
            output_file.write_text("{}\n")
            result = MagicMock()
            result.returncode = 0
            return result

        with patch("ce_runner.pipeline.subprocess.run", side_effect=mock_run):
            result = phase_convert(
                str(session_file), str(task_yaml), str(output_file),
                audit_data_path=str(audit_file),
            )
        assert result is True

    def test_no_audit_data_path_omits_flag(self, tmp_path):
        """phase_convert omits --audit-data when not provided."""
        from ce_runner.pipeline import phase_convert

        session_file = tmp_path / "session.jsonl"
        session_file.write_text('{"type": "message"}\n')

        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices: []\ntools: []\n")

        output_file = tmp_path / "output.jsonl"

        def mock_run(cmd, **kwargs):
            assert "--audit-data" not in cmd
            output_file.write_text("{}\n")
            result = MagicMock()
            result.returncode = 0
            return result

        with patch("ce_runner.pipeline.subprocess.run", side_effect=mock_run):
            phase_convert(str(session_file), str(task_yaml), str(output_file))
