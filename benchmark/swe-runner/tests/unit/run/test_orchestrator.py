# pyright: reportMissingImports=false

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

"""Tests for the orchestrator pipeline."""

from __future__ import annotations

import json
import sys
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "src"))

from swe_runner.agents.cosh.adapter import CoshAdapter
from swe_runner.agents.lifecycle import PreparedAgentRun
from swe_runner.agents.openclaw.adapter import OpenClawAdapter
from swe_runner.common.models import AgentConfig, AgentResult, Settings, SWEInstance
from swe_runner.run.execution.orchestrator import Orchestrator

SAMPLE_DIFF = "diff --git a/foo.py b/foo.py\n--- a/foo.py\n+++ b/foo.py\n@@ -1 +1 @@\n-old\n+new\n"


class FakeAdapter(CoshAdapter):
    def __init__(self, output: str = "FAKE OUTPUT", should_fail: bool = False):
        self._output = output
        self._should_fail = should_fail

    @property
    def name(self) -> str:
        return "fake"

    def run(self, prepared: PreparedAgentRun):
        if self._should_fail:
            raise RuntimeError("Agent failed")
        return AgentResult(raw_output=self._output, patch=None, success=True, duration_seconds=0.1)


class SequenceAdapter(CoshAdapter):
    def __init__(self, outputs: list[str], failures: set[int] | None = None, patches: list[str | None] | None = None):
        self._outputs = outputs
        self._failures = failures or set()
        self._patches = patches
        self._index = 0

    @property
    def name(self) -> str:
        return "fake"

    def run(self, prepared: PreparedAgentRun):
        index = self._index
        self._index += 1
        if index in self._failures:
            raise RuntimeError("Agent failed")
        return AgentResult(raw_output=self._outputs[index], patch=None, success=True, duration_seconds=0.1)


class RecordingAdapter(CoshAdapter):
    def __init__(self) -> None:
        self.calls: list[dict[str, object]] = []

    @property
    def name(self) -> str:
        return "recording"

    def run(self, prepared: PreparedAgentRun):
        self.calls.append(
            {
                "prompt": prepared.prompt,
                "work_dir": prepared.work_dir,
                "instance_id": prepared.instance.instance_id,
                "timeout": prepared.timeout,
                "max_turns": prepared.max_turns,
                **prepared.metadata,
            }
        )
        return AgentResult(raw_output="no diff here", patch=None, success=True, duration_seconds=0.1)


def make_instance(instance_id: str) -> SWEInstance:
    return SWEInstance(
        instance_id=instance_id,
        repo="example/repo",
        version="1.0",
        base_commit="abc123",
        problem_statement="Fix the bug",
        patch="",
        test_patch="",
    )


def make_mock_docker(mock_docker_manager):
    docker = mock_docker_manager.return_value
    docker.start.return_value = Path("/tmp/fake_work")
    docker.container_id = "fake-container-id"
    docker.container_name = "swe-django--django-1234"
    return docker


def test_run_single_success(tmp_path: Path) -> None:
    agent = FakeAdapter(output=SAMPLE_DIFF)
    orchestrator = Orchestrator(agent)

    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager") as mock_docker_manager,
        patch("swe_runner.run.workspace.patches.extract_patch", return_value=SAMPLE_DIFF),
    ):
        make_mock_docker(mock_docker_manager)

        result = orchestrator.run_single(make_instance("instance-1"), tmp_path)

    assert result.success is True
    assert result.prediction is not None
    assert result.prediction.model_patch.startswith("diff --git")
    assert result.agent_result.patch == result.prediction.model_patch

    # Per-instance result file should exist
    result_file = tmp_path / "results" / "instance-1.json"
    assert result_file.exists()
    data = json.loads(result_file.read_text())
    assert data["instance_id"] == "instance-1"
    assert data["success"] is True
    assert data["patch_produced"] is True


def test_run_single_agent_failure(tmp_path: Path) -> None:
    agent = FakeAdapter(should_fail=True)
    orchestrator = Orchestrator(agent)

    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager") as mock_docker_manager,
        patch("swe_runner.run.workspace.patches.extract_patch", return_value=None),
    ):
        make_mock_docker(mock_docker_manager)

        result = orchestrator.run_single(make_instance("instance-2"), tmp_path)

    assert result.success is False
    assert result.prediction is None
    assert result.agent_result.error == "Agent failed"


def test_run_single_agent_failure_is_not_success_even_if_patch_exists(tmp_path: Path) -> None:
    agent = FakeAdapter(should_fail=True)
    orchestrator = Orchestrator(agent)

    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager") as mock_docker_manager,
        patch("swe_runner.run.workspace.patches.extract_patch", return_value=SAMPLE_DIFF),
    ):
        make_mock_docker(mock_docker_manager)

        result = orchestrator.run_single(make_instance("instance-agent-failed-with-patch"), tmp_path)

    assert result.success is False
    assert result.prediction is None
    assert result.agent_result.patch == SAMPLE_DIFF
    data = json.loads((tmp_path / "results" / "instance-agent-failed-with-patch.json").read_text())
    assert data["success"] is False
    assert data["patch_produced"] is True


def test_run_single_docker_failure(tmp_path: Path) -> None:
    orchestrator = Orchestrator(FakeAdapter(output=SAMPLE_DIFF))

    with patch("swe_runner.agents.cosh.adapter.DockerManager") as mock_docker_manager:
        docker = mock_docker_manager.return_value
        docker.start.side_effect = RuntimeError("docker unavailable")

        result = orchestrator.run_single(make_instance("instance-3"), tmp_path)

    assert result.success is False
    assert result.prediction is None
    assert result.agent_result.error == "Prepare error: docker unavailable"


def test_run_single_no_patch(tmp_path: Path) -> None:
    orchestrator = Orchestrator(FakeAdapter(output="no diff here"))

    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager") as mock_docker_manager,
        patch("swe_runner.run.workspace.patches.extract_patch", return_value=None),
    ):
        make_mock_docker(mock_docker_manager)

        result = orchestrator.run_single(make_instance("instance-4"), tmp_path)

    assert result.success is False
    assert result.prediction is None
    assert result.agent_result.patch is None


def test_run_single_passes_instance_id_to_agent(tmp_path: Path) -> None:
    agent = RecordingAdapter()
    orchestrator = Orchestrator(agent)

    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager") as mock_docker_manager,
        patch("swe_runner.run.workspace.patches.extract_patch", return_value=None),
    ):
        make_mock_docker(mock_docker_manager)

        orchestrator.run_single(make_instance("instance-forwarded"), tmp_path)

    assert len(agent.calls) == 1
    assert agent.calls[0]["instance_id"] == "instance-forwarded"


def test_run_single_openclaw_uses_case_agent_and_prepared_workspace(tmp_path: Path) -> None:
    base_config = tmp_path / "base-openclaw.json"
    base_config.write_text('{"agents": {"list": [{"id": "main"}]}}', encoding="utf-8")
    profile_link_root = tmp_path / "home"
    profile_link_root.mkdir()

    class RecordingOpenClawAdapter(OpenClawAdapter):
        def __init__(self) -> None:
            super().__init__(base_config_path=base_config, profile_link_root=profile_link_root)
            self.calls: list[dict[str, object]] = []

        def run(self, prepared: PreparedAgentRun):
            self.calls.append(
                {
                    "prompt": prepared.prompt,
                    "work_dir": prepared.work_dir,
                    "instance_id": prepared.instance.instance_id,
                    "timeout": prepared.timeout,
                    "max_turns": prepared.max_turns,
                    **prepared.metadata,
                }
            )
            return AgentResult(raw_output="ok", patch=None, success=True, duration_seconds=0.1)

    with (
        patch("swe_runner.agents.openclaw.adapter.OpenClawSandboxManager") as mock_sandbox_manager_cls,
        patch("swe_runner.agents.openclaw.adapter.default_workspace_root", return_value=tmp_path / "openclaw-work"),
        patch(
            "swe_runner.agents.openclaw.adapter.prepare_workspace_from_image",
            return_value=tmp_path / "openclaw-work/repo",
        ),
        patch("swe_runner.agents.openclaw.adapter.get_git_revision", return_value="base-rev"),
        patch("swe_runner.agents.openclaw.adapter.build_openclaw_prompt", return_value="fix the bug"),
        patch("swe_runner.run.workspace.patches.extract_patch", return_value=None),
    ):
        agent = RecordingOpenClawAdapter()
        orchestrator = Orchestrator(agent)
        orchestrator.run_single(make_instance("instance-openclaw"), tmp_path)

    assert len(agent.calls) == 1
    assert agent.calls[0]["instance_id"] == "instance-openclaw"
    assert isinstance(agent.calls[0]["session_id"], str)
    assert str(agent.calls[0]["session_id"]).startswith("instance-openclaw-")
    assert agent.calls[0]["agent_id"] == "instance-openclaw"
    assert agent.calls[0]["base_agent_id"] == "swebench"
    assert agent.calls[0]["prompt"] == "fix the bug"
    assert str(agent.calls[0]["openclaw_profile"]).startswith("swebench-instance-openclaw-")
    sandbox_manager = mock_sandbox_manager_cls.return_value
    sandbox_manager.configure.assert_called_once()
    spec = sandbox_manager.configure.call_args.args[0]
    assert spec.agent_id == "instance-openclaw"
    assert spec.image_name == "swebench/sweb.eval.x86_64.instance-openclaw:latest"
    assert spec.workspace_root == tmp_path / "openclaw-work/openclaw-workspace"
    assert spec.testbed_dir == tmp_path / "openclaw-work/repo"
    sandbox_manager.remove_agent_containers.assert_called_once_with("instance-openclaw")


def test_run_single_openclaw_extracts_patch_once_in_runner_tail(tmp_path: Path) -> None:
    base_config = tmp_path / "base-openclaw.json"
    base_config.write_text('{"agents": {"list": [{"id": "main"}]}}', encoding="utf-8")
    profile_link_root = tmp_path / "home"
    profile_link_root.mkdir()

    class RecordingOpenClawAdapter(OpenClawAdapter):
        def __init__(self) -> None:
            super().__init__(base_config_path=base_config, profile_link_root=profile_link_root)

        def run(self, prepared: PreparedAgentRun):
            return AgentResult(raw_output="ok", patch=None, success=True, duration_seconds=0.1)

    with (
        patch("swe_runner.agents.openclaw.adapter.OpenClawSandboxManager"),
        patch("swe_runner.agents.openclaw.adapter.default_workspace_root", return_value=tmp_path / "openclaw-work"),
        patch(
            "swe_runner.agents.openclaw.adapter.prepare_workspace_from_image",
            return_value=tmp_path / "openclaw-work/repo",
        ),
        patch("swe_runner.agents.openclaw.adapter.get_git_revision", return_value="base-rev"),
        patch("swe_runner.agents.openclaw.adapter.build_openclaw_prompt", return_value="fix the bug"),
        patch("swe_runner.run.workspace.patches.extract_patch", return_value=SAMPLE_DIFF) as mock_extract_patch,
    ):
        agent = RecordingOpenClawAdapter()
        orchestrator = Orchestrator(agent)
        result = orchestrator.run_single(make_instance("instance-openclaw-patch"), tmp_path)

    assert result.success is True
    assert result.prediction is not None
    assert result.prediction.model_patch == SAMPLE_DIFF
    assert result.agent_result.patch == SAMPLE_DIFF
    mock_extract_patch.assert_called_once()


def test_run_batch_all_succeed(tmp_path: Path) -> None:
    orchestrator = Orchestrator(SequenceAdapter(outputs=[SAMPLE_DIFF, SAMPLE_DIFF, SAMPLE_DIFF]))
    instances = [make_instance(f"instance-{i}") for i in range(3)]

    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager") as mock_docker_manager,
        patch("swe_runner.run.workspace.patches.extract_patch", return_value=SAMPLE_DIFF),
    ):
        make_mock_docker(mock_docker_manager)

        results = orchestrator.run_batch(instances, tmp_path)

    assert len(results) == 3
    assert all(result.success for result in results)


def test_run_batch_mixed_results(tmp_path: Path) -> None:
    orchestrator = Orchestrator(SequenceAdapter(outputs=[SAMPLE_DIFF, "no diff here", SAMPLE_DIFF]))
    instances = [make_instance(f"instance-{i}") for i in range(3)]

    extract_side_effects = [SAMPLE_DIFF, None, SAMPLE_DIFF]

    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager") as mock_docker_manager,
        patch("swe_runner.run.workspace.patches.extract_patch", side_effect=extract_side_effects),
    ):
        make_mock_docker(mock_docker_manager)

        results = orchestrator.run_batch(instances, tmp_path)

    assert len(results) == 3
    assert sum(result.success for result in results) == 2
    assert sum(not result.success for result in results) == 1


def test_docker_cleanup_always_called(tmp_path: Path) -> None:
    orchestrator = Orchestrator(FakeAdapter(should_fail=True))

    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager") as mock_docker_manager,
        patch("swe_runner.run.workspace.patches.extract_patch", return_value=None),
    ):
        docker = make_mock_docker(mock_docker_manager)

        orchestrator.run_single(make_instance("instance-5"), tmp_path)

    docker.cleanup.assert_called_once_with()


def test_output_files_created(tmp_path: Path) -> None:
    orchestrator = Orchestrator(FakeAdapter(output=SAMPLE_DIFF))

    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager") as mock_docker_manager,
        patch("swe_runner.run.workspace.patches.extract_patch", return_value=SAMPLE_DIFF),
    ):
        make_mock_docker(mock_docker_manager)

        orchestrator.run_single(make_instance("instance-6"), tmp_path)

    preds_file = tmp_path / "preds.json"
    assert preds_file.exists()

    data = json.loads(preds_file.read_text())
    assert len(data) == 1
    assert data["instance-6"]["instance_id"] == "instance-6"

    # Per-instance result file should also exist
    result_file = tmp_path / "results" / "instance-6.json"
    assert result_file.exists()
    rdata = json.loads(result_file.read_text())
    assert rdata["instance_id"] == "instance-6"
    assert rdata["success"] is True
    assert rdata["patch_produced"] is True
    manifest_file = tmp_path / "input-manifests" / "instance-6" / "input_manifest.json"
    assert manifest_file.exists()
    manifest = json.loads(manifest_file.read_text())
    assert manifest["instance_id"] == "instance-6"
    assert manifest["prompt"]["sha256"]
    assert rdata["metadata"]["input_manifest_path"] == str(manifest_file)


def test_run_batch_concurrent_all_succeed(tmp_path: Path) -> None:
    orchestrator = Orchestrator(
        SequenceAdapter(outputs=[SAMPLE_DIFF, SAMPLE_DIFF, SAMPLE_DIFF]),
        Settings(agent=AgentConfig(name="fake", workers=4)),
    )
    instances = [make_instance(f"concurrent-{i}") for i in range(3)]

    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager") as mock_docker_manager,
        patch("swe_runner.run.workspace.patches.extract_patch", return_value=SAMPLE_DIFF),
    ):
        make_mock_docker(mock_docker_manager)
        results = orchestrator.run_batch(instances, tmp_path)

    assert len(results) == 3
    assert all(r.success for r in results)


def test_run_batch_concurrent_mixed_results(tmp_path: Path) -> None:
    orchestrator = Orchestrator(
        SequenceAdapter(outputs=[SAMPLE_DIFF, "no diff", SAMPLE_DIFF]),
        Settings(agent=AgentConfig(name="fake", workers=3)),
    )
    instances = [make_instance(f"mixed-{i}") for i in range(3)]

    extract_side_effects = [SAMPLE_DIFF, None, SAMPLE_DIFF]

    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager") as mock_docker_manager,
        patch("swe_runner.run.workspace.patches.extract_patch", side_effect=extract_side_effects),
    ):
        make_mock_docker(mock_docker_manager)
        results = orchestrator.run_batch(instances, tmp_path)

    assert len(results) == 3
    assert sum(r.success for r in results) == 2


def test_run_batch_workers_gt_instance_count(tmp_path: Path) -> None:
    """More workers than instances should still work correctly."""
    orchestrator = Orchestrator(
        SequenceAdapter(outputs=[SAMPLE_DIFF, SAMPLE_DIFF]),
        Settings(agent=AgentConfig(name="fake", workers=4)),
    )
    instances = [make_instance(f"few-{i}") for i in range(2)]

    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager") as mock_docker_manager,
        patch("swe_runner.run.workspace.patches.extract_patch", return_value=SAMPLE_DIFF),
    ):
        make_mock_docker(mock_docker_manager)
        results = orchestrator.run_batch(instances, tmp_path)

    assert len(results) == 2
    assert all(r.success for r in results)


def test_run_batch_concurrent_empty(tmp_path: Path) -> None:
    """Concurrent mode with empty instance list returns empty results."""
    orchestrator = Orchestrator(
        FakeAdapter(output=SAMPLE_DIFF),
        Settings(agent=AgentConfig(name="fake", workers=4)),
    )

    with patch("swe_runner.agents.cosh.adapter.DockerManager"):
        results = orchestrator.run_batch([], tmp_path)

    assert len(results) == 0


def test_run_batch_concurrent_docker_cleanup(tmp_path: Path) -> None:
    """Docker cleanup is called for every instance in concurrent mode."""
    orchestrator = Orchestrator(
        SequenceAdapter(outputs=[SAMPLE_DIFF, "no diff", SAMPLE_DIFF]),
        Settings(agent=AgentConfig(name="fake", workers=3)),
    )
    instances = [make_instance(f"cleanup-{i}") for i in range(3)]

    extract_side_effects = [SAMPLE_DIFF, None, SAMPLE_DIFF]

    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager") as mock_docker_manager,
        patch("swe_runner.run.workspace.patches.extract_patch", side_effect=extract_side_effects),
    ):
        make_mock_docker(mock_docker_manager)
        orchestrator.run_batch(instances, tmp_path)

    assert mock_docker_manager.return_value.cleanup.call_count == 3


def test_run_batch_skips_attempted(tmp_path: Path) -> None:
    """Instances with existing result files are skipped when redo=False."""
    results_dir = tmp_path / "results"
    results_dir.mkdir()
    # Write result files for instance-0 (success) and instance-1 (failure)
    (results_dir / "instance-0.json").write_text(
        json.dumps(
            {
                "instance_id": "instance-0",
                "success": True,
                "error": None,
                "duration_seconds": 10.0,
                "patch_produced": True,
                "agent_name": "fake",
            }
        )
    )
    (results_dir / "instance-1.json").write_text(
        json.dumps(
            {
                "instance_id": "instance-1",
                "success": False,
                "error": "Timeout",
                "duration_seconds": 5.0,
                "patch_produced": False,
                "agent_name": "fake",
            }
        )
    )

    agent = SequenceAdapter(outputs=[SAMPLE_DIFF])
    orchestrator = Orchestrator(agent)
    instances = [make_instance(f"instance-{i}") for i in range(3)]

    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager") as mock_docker_manager,
        patch("swe_runner.run.workspace.patches.extract_patch", return_value=SAMPLE_DIFF),
    ):
        make_mock_docker(mock_docker_manager)
        results = orchestrator.run_batch(instances, tmp_path)

    # Only instance-2 should have been run (instance-0 and instance-1 already attempted)
    assert len(results) == 1
    assert results[0].instance.instance_id == "instance-2"
    assert mock_docker_manager.call_count == 1

    # preds.json should contain instance-2 (the only newly run instance)
    preds_file = tmp_path / "preds.json"
    preds = json.loads(preds_file.read_text())
    assert "instance-2" in preds


def test_run_batch_redo_flag(tmp_path: Path) -> None:
    """With redo=True, all instances are rerun even if result files exist."""
    results_dir = tmp_path / "results"
    results_dir.mkdir()
    (results_dir / "instance-0.json").write_text(
        json.dumps(
            {
                "instance_id": "instance-0",
                "success": True,
                "error": None,
                "duration_seconds": 10.0,
                "patch_produced": True,
                "agent_name": "old",
            }
        )
    )

    agent = SequenceAdapter(outputs=[SAMPLE_DIFF, SAMPLE_DIFF, SAMPLE_DIFF])
    orchestrator = Orchestrator(agent, redo=True)
    instances = [make_instance(f"instance-{i}") for i in range(3)]

    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager") as mock_docker_manager,
        patch("swe_runner.run.workspace.patches.extract_patch", return_value=SAMPLE_DIFF),
    ):
        make_mock_docker(mock_docker_manager)
        results = orchestrator.run_batch(instances, tmp_path)

    assert len(results) == 3
    assert all(r.success for r in results)
    assert mock_docker_manager.call_count == 3


def test_run_batch_openclaw_cleans_local_sandbox_on_failure(tmp_path: Path) -> None:
    base_config = tmp_path / "base-openclaw.json"
    base_config.write_text('{"agents": {"list": [{"id": "main"}]}}', encoding="utf-8")
    profile_link_root = tmp_path / "home"
    profile_link_root.mkdir()

    class FailingOpenClawAdapter(OpenClawAdapter):
        def __init__(self) -> None:
            super().__init__(base_config_path=base_config, profile_link_root=profile_link_root)

        def run(self, prepared: PreparedAgentRun):
            raise RuntimeError("boom")

    with (
        patch("swe_runner.agents.openclaw.adapter.OpenClawSandboxManager") as mock_sandbox_manager_cls,
        patch("swe_runner.agents.openclaw.adapter.default_workspace_root", return_value=tmp_path / "openclaw-work"),
        patch(
            "swe_runner.agents.openclaw.adapter.prepare_workspace_from_image",
            return_value=tmp_path / "openclaw-work/repo",
        ),
        patch("swe_runner.agents.openclaw.adapter.get_git_revision", return_value="base-rev"),
        patch("swe_runner.agents.openclaw.adapter.build_openclaw_prompt", return_value="fix the bug"),
        patch("swe_runner.run.workspace.patches.extract_patch", return_value=None),
    ):
        orchestrator = Orchestrator(FailingOpenClawAdapter())
        results = orchestrator.run_batch([make_instance("instance-openclaw")], tmp_path)

    assert len(results) == 1
    assert results[0].success is False
    sandbox_manager = mock_sandbox_manager_cls.return_value
    sandbox_manager.configure.assert_called_once()
    sandbox_manager.remove_agent_containers.assert_called_once_with("instance-openclaw")
