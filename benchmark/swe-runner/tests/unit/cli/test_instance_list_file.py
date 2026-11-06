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

"""Reusable file selections are validated before agent or dataset execution."""

from pathlib import Path
from typing import Any
from unittest.mock import MagicMock

import pytest
from typer.testing import CliRunner

from swe_runner.cli import app
from swe_runner.cli_commands import CommandUsageError, build_run_settings
from swe_runner.common.models import Settings, SWEInstance
from swe_runner.run.dataset import filter_instances


def _settings(tmp_path: Path, selection: Path, **overrides: Any) -> Settings:
    kwargs = dict(
        agent="cosh",
        subset="lite",
        split="test",
        output=tmp_path,
        timeout=120,
        step_limit=0,
        slice_range=None,
        filter_regex=None,
        instance_id=None,
        workers=1,
        docker_pull_registry=None,
        use_skill=False,
        tokenless=False,
        per_case_prompt=False,
        instances_file=selection,
    )
    kwargs.update(overrides)
    return build_run_settings(**kwargs)


def test_bom_comments_whitespace_and_duplicates_are_normalized(tmp_path: Path) -> None:
    selection = tmp_path / "ids.txt"
    selection.write_text("\ufeff# Reviewed subset\n\n beta \nalpha\nbeta\n # note\n", encoding="utf-8")
    settings = _settings(tmp_path, selection)
    assert settings.dataset.instance_ids == ["beta", "alpha"]


def test_unicode_ids_are_read_as_utf8(tmp_path: Path) -> None:
    selection = tmp_path / "ids.txt"
    selection.write_text("项目-case\ncase-🌱\n", encoding="utf-8")
    assert _settings(tmp_path, selection).dataset.instance_ids == ["项目-case", "case-🌱"]


@pytest.mark.parametrize("contents", ["", " \n# no IDs\n"])
def test_empty_selections_are_rejected(tmp_path: Path, contents: str) -> None:
    selection = tmp_path / "empty.txt"
    selection.write_text(contents, encoding="utf-8")
    with pytest.raises(CommandUsageError, match="contains no instance IDs"):
        _settings(tmp_path, selection)


@pytest.mark.parametrize("kind", ["missing", "directory", "invalid-utf8"])
def test_unreadable_selections_report_the_path(tmp_path: Path, kind: str) -> None:
    selection = tmp_path / "ids.txt"
    if kind == "directory":
        selection.mkdir()
    elif kind == "invalid-utf8":
        selection.write_bytes(b"\xff")
    with pytest.raises(CommandUsageError, match="Cannot read --instances-file") as caught:
        _settings(tmp_path, selection)
    assert str(selection) in str(caught.value)


def test_conflicting_id_sources_are_rejected_before_file_access(tmp_path: Path) -> None:
    with pytest.raises(CommandUsageError, match="mutually exclusive"):
        _settings(tmp_path, tmp_path / "missing.txt", instance_id="alpha")


def test_dataset_order_then_existing_regex_and_slice_are_retained(tmp_path: Path) -> None:
    selection = tmp_path / "ids.txt"
    selection.write_text("case-3\ncase-2\ncase-1\n", encoding="utf-8")
    settings = _settings(tmp_path, selection, filter_regex="case-[13]", slice_range="1:")
    instances = [
        SWEInstance(
            instance_id=name,
            repo="demo/repo",
            version="1",
            base_commit="abc",
            problem_statement="issue",
            patch="",
            test_patch="",
        )
        for name in ["case-1", "case-2", "case-3", "other"]
    ]
    assert [instance.instance_id for instance in filter_instances(instances, settings.dataset)] == ["case-3"]


def test_cli_snapshots_file_ids_into_run_settings(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    selection = tmp_path / "ids.txt"
    selection.write_text("alpha\nbeta\n", encoding="utf-8")
    session = MagicMock()
    session.return_value.execute.return_value.total = 0
    monkeypatch.setattr("swe_runner.cli_commands.RunSession", session)
    monkeypatch.setattr("swe_runner.cli_commands.setup_logging", lambda *args, **kwargs: None)
    result = CliRunner().invoke(app, ["run", "--agent", "cosh", "--instances-file", str(selection)])
    assert result.exit_code == 0, result.output
    assert session.call_args.args[0].dataset.instance_ids == ["alpha", "beta"]


def test_cli_invalid_file_stops_before_a_run(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    session = MagicMock()
    monkeypatch.setattr("swe_runner.cli_commands.RunSession", session)
    result = CliRunner().invoke(app, ["run", "--agent", "cosh", "--instances-file", str(tmp_path / "missing.txt")])
    assert result.exit_code == 1
    assert "Cannot read --instances-file" in result.output
    session.assert_not_called()
