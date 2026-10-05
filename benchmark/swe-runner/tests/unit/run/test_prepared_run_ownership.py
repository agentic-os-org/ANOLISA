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

"""Prepared resource ownership across actual single-instance execution."""

from __future__ import annotations

import shutil
from contextlib import ExitStack
from pathlib import Path
from unittest.mock import patch

import pytest

from swe_runner.agents import AgentAdapter, PreparedAgentRun
from swe_runner.common.models import AgentResult, Settings, SWEInstance
from swe_runner.run.execution.instance_runner import run_instance


class FixtureAdapter(AgentAdapter):
    def __init__(self, work_dir: Path, *, run_error: BaseException | None = None) -> None:
        self.instance = SWEInstance(
            instance_id="owned-run",
            repo="fixture/repo",
            version="1",
            base_commit="abc",
            problem_statement="private fixture",
            patch="",
            test_patch="",
        )
        self.work_dir = work_dir
        self.work_dir.mkdir()
        (self.work_dir / "owned.txt").write_text("owned", encoding="utf-8")
        self.cleanup_calls: list[str] = []
        self.run_error = run_error
        self.prepared = PreparedAgentRun(
            instance=self.instance,
            settings=Settings(),
            work_dir=self.work_dir,
            prompt="private fixture",
            timeout=30,
            max_turns=0,
            cleanup_callbacks=[self.release_work_dir, lambda: self.cleanup_calls.append("container")],
        )

    @property
    def name(self) -> str:
        return "fixture"

    def prepare(self, instance: SWEInstance, settings: Settings) -> PreparedAgentRun:
        assert instance is self.instance
        return self.prepared

    def run(self, prepared: PreparedAgentRun) -> AgentResult:
        assert prepared is self.prepared
        assert self.work_dir.is_dir()
        if self.run_error is not None:
            raise self.run_error
        return AgentResult(raw_output="fixture", patch=None, success=True, duration_seconds=0)

    def release_work_dir(self) -> None:
        self.cleanup_calls.append("work_dir")
        shutil.rmtree(self.work_dir)


@pytest.mark.parametrize("boundary", ["manifest", "manifest_metadata", "agent", "merge", "finish_log"])
@pytest.mark.parametrize("error_type", [RuntimeError, KeyboardInterrupt, SystemExit])
def test_exception_before_finalization_releases_owned_resources(
    tmp_path: Path,
    boundary: str,
    error_type: type[BaseException],
) -> None:
    error = error_type("primary execution failure")
    adapter = FixtureAdapter(tmp_path / "workspace", run_error=error if boundary == "agent" else None)
    prefix = "swe_runner.run.execution.instance_lifecycle."

    def fail_finish(message: str, *args: object) -> None:
        if message.startswith("AGENT_FINISH"):
            raise error

    targets = {
        "manifest": (prefix + "write_input_manifest", {"side_effect": error}),
        "manifest_metadata": (prefix + "RunArtifacts.from_metadata", {"side_effect": error}),
        "agent": (prefix + "time.monotonic", {"return_value": 1}),
        "merge": (prefix + "merge_metadata", {"side_effect": error}),
        "finish_log": (prefix + "logger.info", {"side_effect": fail_finish}),
    }
    target, kwargs = targets[boundary]
    with ExitStack() as stack:
        stack.enter_context(patch(target, **kwargs))
        if boundary == "manifest_metadata":
            stack.enter_context(patch(prefix + "write_input_manifest", return_value=tmp_path / "manifest.json"))
        if error_type is RuntimeError and boundary in {"manifest", "agent"}:
            with patch("swe_runner.run.execution.finalization.patches.extract_patch", return_value=""):
                result = run_instance(adapter, Settings(), adapter.instance, tmp_path / "output")
            assert not result.success
        else:
            with pytest.raises(error_type) as raised:
                run_instance(adapter, Settings(), adapter.instance, tmp_path / "output")
            assert raised.value is error
    assert adapter.cleanup_calls == ["container", "work_dir"]
    assert adapter.prepared.cleanup_callbacks == []
    assert not adapter.work_dir.exists()


@pytest.mark.parametrize("patch_error", [None, RuntimeError("patch failed"), KeyboardInterrupt("patch interrupted")])
def test_finalization_keeps_workspace_until_patch_then_releases_once(
    tmp_path: Path, patch_error: BaseException | None
) -> None:
    adapter = FixtureAdapter(tmp_path / "workspace")

    def extract(*args: object, **kwargs: object) -> str:
        assert (adapter.work_dir / "owned.txt").is_file()
        if patch_error is not None:
            raise patch_error
        return "diff --git a/file b/file\nfixture patch\n"

    with patch("swe_runner.run.execution.finalization.patches.extract_patch", side_effect=extract):
        if isinstance(patch_error, KeyboardInterrupt):
            with pytest.raises(KeyboardInterrupt) as raised:
                run_instance(adapter, Settings(), adapter.instance, tmp_path / "output")
            assert raised.value is patch_error
        else:
            result = run_instance(adapter, Settings(), adapter.instance, tmp_path / "output")
            assert result.success is (patch_error is None)
            assert (tmp_path / "output/results/owned-run.json").is_file()
    assert adapter.cleanup_calls == ["container", "work_dir"]
    assert not adapter.work_dir.exists()


def test_cleanup_and_diagnostic_failure_do_not_replace_execution_interrupt(tmp_path: Path) -> None:
    error = KeyboardInterrupt("original interrupt")
    adapter = FixtureAdapter(tmp_path / "workspace", run_error=error)
    with (
        patch.object(adapter.prepared, "cleanup", side_effect=RuntimeError("cleanup failed")) as cleanup,
        patch("swe_runner.agents.lifecycle.logger.exception", side_effect=OSError("log failed")),
        pytest.raises(KeyboardInterrupt) as raised,
    ):
        run_instance(adapter, Settings(), adapter.instance, tmp_path / "output")
    assert raised.value is error
    cleanup.assert_called_once_with()


@pytest.mark.parametrize("secondary_type", [KeyboardInterrupt, SystemExit])
@pytest.mark.parametrize("diagnostic_fails", [False, True])
@pytest.mark.parametrize("boundary", ["agent", "patch"])
def test_secondary_callback_interrupt_still_releases_remaining_resource(
    tmp_path: Path,
    secondary_type: type[BaseException],
    diagnostic_fails: bool,
    boundary: str,
) -> None:
    primary = KeyboardInterrupt("primary execution interrupt")
    adapter = FixtureAdapter(tmp_path / "workspace", run_error=primary if boundary == "agent" else None)

    def release_container() -> None:
        adapter.cleanup_calls.append("container")
        raise secondary_type("secondary cleanup interrupt")

    adapter.prepared.cleanup_callbacks = [adapter.release_work_dir, release_container]
    with (
        patch(
            "swe_runner.agents.lifecycle.logger.exception",
            side_effect=OSError("diagnostic failed") if diagnostic_fails else None,
        ),
        patch("swe_runner.run.execution.finalization.patches.extract_patch", side_effect=primary),
        pytest.raises(KeyboardInterrupt) as raised,
    ):
        run_instance(adapter, Settings(), adapter.instance, tmp_path / "output")
    assert raised.value is primary
    assert adapter.cleanup_calls == ["container", "work_dir"]
    assert adapter.prepared.cleanup_callbacks == []
    assert not adapter.work_dir.exists()
