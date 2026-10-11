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

import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "list_tasks.py"
SPEC = importlib.util.spec_from_file_location("task_inventory_formats", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


@pytest.fixture
def tasks(tmp_path: Path) -> Path:
    root = tmp_path / "custom-tasks"
    for name, task_id, difficulty in (
        ("directory-alpha", "T002", "hard"),
        ("directory-beta", "T001", "easy"),
        ("directory-gamma", "M001", "hard"),
    ):
        directory = root / name
        directory.mkdir(parents=True)
        (directory / "task.yaml").write_text(
            f"task_id: {task_id}\ntask_name: 邮件整理\ndifficulty: {difficulty}\n",
            encoding="utf-8",
        )
    return root


def test_default_grouped_output_stays_available(
    tasks: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(MODULE, "TASKS_DIR", tasks)
    monkeypatch.setattr(sys, "argv", ["list_tasks.py"])
    MODULE.main()
    output = capsys.readouterr().out
    assert "Prefix: T" in output and "Prefix: M" in output
    assert "[hard]" in output and "[easy]" in output
    assert "Grand total: 3 tasks" in output


def test_json_inventory_uses_custom_root_and_metadata(
    tasks: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "list_tasks.py",
            "--tasks-dir",
            str(tasks),
            "--format",
            "json",
            "--prefix",
            "T",
            "--difficulty",
            "hard",
        ],
    )
    MODULE.main()
    rows = json.loads(capsys.readouterr().out)
    assert len(rows) == 1
    assert rows[0]["task_id"] == "T002"
    assert rows[0]["directory_name"] == "directory-alpha"
    assert rows[0]["task_name"] == "邮件整理"
    assert rows[0]["difficulty"] == "hard"


def test_names_inventory_selects_exact_directory_names(
    tasks: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "list_tasks.py",
            "--tasks-dir",
            str(tasks),
            "--format",
            "names",
            "--prefix",
            "T",
        ],
    )
    MODULE.main()
    assert capsys.readouterr().out.splitlines() == ["directory-alpha", "directory-beta"]


@pytest.mark.parametrize("format_name, expected", [("json", "[]\n"), ("names", "")])
def test_empty_machine_inventory_has_no_human_message(
    tasks: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    format_name: str,
    expected: str,
) -> None:
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "list_tasks.py",
            "--tasks-dir",
            str(tasks),
            "--format",
            format_name,
            "--prefix",
            "C",
        ],
    )
    MODULE.main()
    assert capsys.readouterr().out == expected


@pytest.mark.parametrize("format_name", ["json", "names"])
def test_direct_script_runs_from_another_directory(
    tasks: Path, tmp_path: Path, format_name: str
) -> None:
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--tasks-dir",
            str(tasks),
            "--format",
            format_name,
        ],
        cwd=tmp_path,
        env={**os.environ, "PYTHONIOENCODING": "utf-8"},
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert result.stderr == ""
    if format_name == "json":
        assert len(json.loads(result.stdout)) == 3
    else:
        assert result.stdout.splitlines() == [
            "directory-alpha",
            "directory-beta",
            "directory-gamma",
        ]


def test_custom_root_file_is_an_actionable_error(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    path = tmp_path / "not-a-directory"
    path.write_text("file", encoding="utf-8")
    monkeypatch.setattr(
        sys, "argv", ["list_tasks.py", "--tasks-dir", str(path), "--format", "json"]
    )
    with pytest.raises(SystemExit) as error:
        MODULE.main()
    assert error.value.code == 1
    output = capsys.readouterr()
    assert output.out == ""
    assert str(path) in output.err
    assert "tasks directory" in output.err


@pytest.mark.parametrize("format_name", ["json", "names"])
def test_machine_output_is_utf8_with_ascii_stdout(
    tasks: Path, tmp_path: Path, format_name: str
) -> None:
    directory = tasks / "目录😀"
    directory.mkdir()
    (directory / "task.yaml").write_text(
        "task_id: M999\ntask_name: 邮件😀\ndifficulty: hard\n", encoding="utf-8"
    )
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--tasks-dir",
            str(tasks),
            "--format",
            format_name,
            "--prefix",
            "M",
        ],
        cwd=tmp_path,
        env={**os.environ, "PYTHONIOENCODING": "ascii"},
        capture_output=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr.decode("ascii", errors="replace")
    output = result.stdout.decode("utf-8")
    if format_name == "json":
        assert any(row["task_name"] == "邮件😀" for row in json.loads(output))
    else:
        assert output.splitlines() == ["directory-gamma", "目录😀"]
