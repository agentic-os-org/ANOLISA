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

from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock

import pytest
from ce_runner import batch_runner


def _task(root: Path, directory: str, content: str) -> Path:
    path = root / directory
    path.mkdir(parents=True)
    (path / "task.yaml").write_text(content, encoding="utf-8")
    return path


def _run_selected(
    root: Path,
    names: str | None,
    monkeypatch: pytest.MonkeyPatch,
    tasks_file: Path | None = None,
) -> Mock:
    args = SimpleNamespace(
        tasks_dir=str(root),
        tag=None,
        parallel=2,
        config=None,
        timeout=60,
        trials=1,
        tasks_string=names,
        tasks_file=str(tasks_file) if tasks_file else None,
    )
    monkeypatch.setattr(batch_runner, "require_valid_config", lambda *args: None)
    gateway = Mock(
        side_effect=AssertionError("gateway queried before identity validation")
    )
    monkeypatch.setattr(batch_runner, "check_gateway", gateway)
    batch_runner.run_batch(args, lambda cfg: {}, lambda cfg: {}, lambda cfg: {}, Mock())
    return gateway


def test_duplicate_selection_is_rejected_before_gateway(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    _task(tmp_path, "T001", "task_id: T001\n")
    with pytest.raises(SystemExit) as error:
        _run_selected(tmp_path, "T001,T001", monkeypatch)
    assert error.value.code == 1
    output = capsys.readouterr().out
    assert "Duplicate task selection" in output
    assert "T001" in output
    assert "--trials" in output


def test_different_yaml_paths_cannot_share_task_id(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    _task(tmp_path, "T001", "task_id: shared\n")
    _task(tmp_path, "T002", "task_id: shared\n")
    with pytest.raises(SystemExit) as error:
        _run_selected(tmp_path, "T001,T002", monkeypatch)
    assert error.value.code == 1
    output = capsys.readouterr().out
    assert "Duplicate task_id 'shared'" in output
    assert "T001" in output and "T002" in output


@pytest.mark.parametrize(
    "content",
    ["task_name: missing\n", "task_id: null\n", "task_id: 42\n", "task_id: ' '\n"],
)
def test_missing_or_unusable_id_stops_before_gateway(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    content: str,
) -> None:
    _task(tmp_path, "T001", content)
    with pytest.raises(SystemExit) as error:
        _run_selected(tmp_path, "T001", monkeypatch)
    assert error.value.code == 1
    assert "nonempty string task_id" in capsys.readouterr().out


def test_invalid_yaml_stops_before_gateway(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    _task(tmp_path, "T001", "task_id: [broken\n")
    with pytest.raises(SystemExit) as error:
        _run_selected(tmp_path, "T001", monkeypatch)
    assert error.value.code == 1
    assert "Invalid batch task inputs" in capsys.readouterr().out


def test_valid_task_order_and_metadata_are_preserved(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    first = _task(
        tmp_path, "T002", "task_id: T002\ntask_name: second\ndifficulty: hard\n"
    )
    second = _task(tmp_path, "T001", "task_id: T001\n")
    with pytest.raises(AssertionError, match="gateway queried"):
        _run_selected(tmp_path, "T002,T001", monkeypatch)
    yamls, directory_map, metadata = batch_runner._collect_task_inputs(
        [str(first), str(second)]
    )
    assert yamls == [str(first / "task.yaml"), str(second / "task.yaml")]
    assert directory_map == {yamls[0]: str(first), yamls[1]: str(second)}
    assert metadata == {
        "T002": {"task_name": "second", "difficulty": "hard"},
        "T001": {"task_name": "", "difficulty": ""},
    }


def test_duplicate_tasks_file_is_rejected(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    _task(tmp_path, "T001", "task_id: T001\n")
    tasks_file = tmp_path / "tasks.txt"
    tasks_file.write_text("T001\nT001\n", encoding="utf-8")
    with pytest.raises(SystemExit) as error:
        _run_selected(tmp_path, None, monkeypatch, tasks_file)
    assert error.value.code == 1
    assert "Duplicate task selection" in capsys.readouterr().out


def test_directory_aliases_do_not_bypass_identity_check(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    original = _task(tmp_path, "T001", "task_id: T001\n")
    (tmp_path / "T001-alias").symlink_to(original, target_is_directory=True)
    with pytest.raises(SystemExit) as error:
        _run_selected(tmp_path, "T001,T001-alias", monkeypatch)
    assert error.value.code == 1
    assert "Duplicate task selection" in capsys.readouterr().out
