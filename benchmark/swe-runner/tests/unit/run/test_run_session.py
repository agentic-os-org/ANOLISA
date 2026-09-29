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

"""Unit tests for RunSession and RunReport."""

from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from swe_runner.agents import AgentEnvironmentError
from swe_runner.common.models import (
    AgentConfig,
    AgentResult,
    DatasetConfig,
    InstanceResult,
    OutputConfig,
    Settings,
    SWEInstance,
)
from swe_runner.run.io.report import RunReport
from swe_runner.run.session import RunSession


def _make_instance(instance_id: str = "test-1") -> SWEInstance:
    return SWEInstance(
        instance_id=instance_id,
        repo="test/repo",
        version="1.0",
        base_commit="abc123",
        problem_statement="fix bug",
        patch="diff",
        test_patch="test diff",
    )


def _make_result(
    instance_id: str = "test-1",
    success: bool = True,
    session_id: str | None = None,
    profile_dir: str | None = None,
) -> InstanceResult:
    metadata: dict[str, str] = {}
    if session_id:
        metadata["session_id"] = session_id
    if profile_dir:
        metadata["openclaw_profile_dir"] = profile_dir
    return InstanceResult(
        instance=_make_instance(instance_id),
        prediction=None,
        agent_result=AgentResult(
            raw_output="output",
            patch="diff" if success else None,
            success=success,
            duration_seconds=1.0,
            metadata=metadata,
        ),
        success=success,
    )


class TestRunReport:
    """Tests for RunReport.from_results metadata aggregation."""

    def test_from_results_basic(self):
        results = [_make_result("i1", success=True), _make_result("i2", success=False)]
        report = RunReport.from_results(results, started_at_ns=100, ended_at_ns=200)

        assert report.succeeded == 1
        assert report.failed == 1
        assert report.total == 2
        assert report.instance_ids == ["i1", "i2"]
        assert report.started_at_ns == 100
        assert report.ended_at_ns == 200

    def test_from_results_extracts_session_ids(self):
        results = [
            _make_result("i1", session_id="sess-1"),
            _make_result("i2"),
        ]
        report = RunReport.from_results(results, started_at_ns=0, ended_at_ns=0)

        assert report.metadata_mappings["session_ids"] == {"i1": "sess-1"}

    def test_from_results_extracts_profile_dirs(self):
        results = [
            _make_result("i1", profile_dir="/tmp/p1"),
            _make_result("i2", profile_dir="/tmp/p2"),
        ]
        report = RunReport.from_results(results, started_at_ns=0, ended_at_ns=0)

        assert report.metadata_mappings["openclaw_profile_dirs"] == {"i1": "/tmp/p1", "i2": "/tmp/p2"}

    def test_from_results_empty(self):
        report = RunReport.from_results([], started_at_ns=0, ended_at_ns=0)
        assert report.total == 0
        assert report.instance_ids == []


class TestRunSession:
    """Tests for RunSession.execute() lifecycle."""

    def _make_settings(self, tmp_path: Path, agent_name: str = "cosh") -> Settings:
        return Settings(
            agent=AgentConfig(name=agent_name),
            dataset=DatasetConfig(subset="lite", split="test"),
            output=OutputConfig(output_dir=tmp_path),
        )

    def test_execute_env_check_failure_propagates(self, tmp_path):
        settings = self._make_settings(tmp_path)
        session = RunSession(settings)

        with (
            patch(
                "swe_runner.run.session.check_agent_environment",
                side_effect=AgentEnvironmentError("Docker not running"),
            ),
            pytest.raises(AgentEnvironmentError, match="Docker not running"),
        ):
            session.execute()

    def test_execute_unknown_agent_propagates(self, tmp_path):
        settings = self._make_settings(tmp_path, agent_name="nonexistent")
        session = RunSession(settings)

        with (
            patch("swe_runner.run.session.check_agent_environment"),
            patch("swe_runner.run.session.get_agent", side_effect=KeyError("Unknown agent")),
            pytest.raises(KeyError),
        ):
            session.execute()

    def test_execute_no_instances_returns_empty_report(self, tmp_path):
        settings = self._make_settings(tmp_path)
        session = RunSession(settings)

        with (
            patch("swe_runner.run.session.check_agent_environment"),
            patch("swe_runner.run.session.get_agent", return_value=MagicMock()),
            patch("swe_runner.run.session.load_dataset", return_value=[]),
        ):
            report = session.execute()

        assert report.total == 0
        assert report.metadata_path is None

    def test_execute_success_writes_metadata(self, tmp_path):
        settings = self._make_settings(tmp_path)
        session = RunSession(settings)
        results = [_make_result("i1", session_id="sess-1")]

        with (
            patch("swe_runner.run.session.check_agent_environment"),
            patch("swe_runner.run.session.get_agent", return_value=MagicMock()),
            patch("swe_runner.run.session.load_dataset", return_value=[_make_instance("i1")]),
            patch("swe_runner.run.session.Orchestrator") as mock_orch_cls,
            patch("swe_runner.run.session.RunOutputStore") as mock_store_cls,
        ):
            mock_store = mock_store_cls.return_value
            mock_store.write_run_metadata.return_value = tmp_path / "run_metadata.json"
            mock_orch_cls.return_value.run_batch.return_value = results
            report = session.execute()

        assert report.succeeded == 1
        assert report.total == 1
        assert report.metadata_path == tmp_path / "run_metadata.json"
        mock_store_cls.assert_called_once_with(tmp_path)
        mock_store.write_run_metadata.assert_called_once()
        snapshot = mock_store.write_run_metadata.call_args.args[0]
        assert snapshot.metadata_mappings == {"session_ids": {"i1": "sess-1"}}

    def test_execute_passes_redo_to_orchestrator(self, tmp_path):
        settings = self._make_settings(tmp_path)
        session = RunSession(settings, redo=True)

        with (
            patch("swe_runner.run.session.check_agent_environment"),
            patch("swe_runner.run.session.get_agent", return_value=MagicMock()),
            patch("swe_runner.run.session.load_dataset", return_value=[_make_instance()]),
            patch("swe_runner.run.session.Orchestrator") as mock_orch_cls,
            patch("swe_runner.run.session.RunOutputStore") as mock_store_cls,
        ):
            mock_store_cls.return_value.write_run_metadata.return_value = tmp_path / "meta.json"
            mock_orch_cls.return_value.run_batch.return_value = [_make_result()]
            session.execute()

        assert mock_orch_cls.call_args.kwargs["redo"] is True
