# SPDX-License-Identifier: Apache-2.0
"""Decode task scalars through real files and the inventory CLI."""

import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/list_tasks.py"
SPEC = importlib.util.spec_from_file_location("inventory_yaml_scalars", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def metadata_file(tmp_path: Path, content: str) -> Path:
    path = tmp_path / "task.yaml"
    path.write_text(content, encoding="utf-8")
    return path


def test_comments_do_not_hide_selected_tasks(tmp_path: Path) -> None:
    directory = tmp_path / "tasks/directory-name"
    directory.mkdir(parents=True)
    metadata_file(
        directory,
        'task_id: "T001" # identifier\n'
        'task_name: "Support: review" # description\n'
        "difficulty: 'hard' # reviewed\n",
    )
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--tasks-dir",
            str(directory.parent),
            "--prefix",
            "T",
            "--difficulty",
            "hard",
            "--format",
            "json",
        ],
        env={**os.environ, "PYTHONIOENCODING": "utf-8"},
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
        timeout=20,
    )
    assert result.returncode == 0, result.stderr
    rows = json.loads(result.stdout)
    assert len(rows) == 1
    assert rows[0]["task_id"] == "T001"
    assert rows[0]["difficulty"] == "hard"
    assert rows[0]["task_name"] == "Support: review"
    assert rows[0]["directory_name"] == "directory-name"


@pytest.mark.parametrize(
    "content, expected",
    [
        ('task_name: "Profit: # margin"\n', "Profit: # margin"),
        ("task_name: 'owner''s mail'\n", "owner's mail"),
        ('task_name: "mail\\nqueue\\u20ac"\n', "mail\nqueue€"),
        (
            "task_name: |\n  first line\n  # literal comment\n",
            "first line\n# literal comment\n",
        ),
        ("task_name: >-\n  first line\n  second line\n", "first line second line"),
        ('task_name: "邮件 😀"\n', "邮件 😀"),
    ],
)
def test_scalar_text_is_decoded(tmp_path: Path, content: str, expected: str) -> None:
    result = MODULE._load_yaml_simple(metadata_file(tmp_path, content))
    assert result["task_name"] == expected


def test_flat_metadata_types_and_flow_spelling_are_retained(tmp_path: Path) -> None:
    result = MODULE._load_yaml_simple(
        metadata_file(
            tmp_path,
            "task_id: T001\ntask_name: 001\ncategory: true\n"
            "tags: [mail, calendar]\nservices:\n  - name: fixture\n",
        )
    )
    assert result == {
        "task_id": "T001",
        "task_name": "001",
        "category": "true",
        "tags": "[mail, calendar]",
        "services": "",
    }


def test_empty_values_and_document_are_retained(tmp_path: Path) -> None:
    assert MODULE._load_yaml_simple(metadata_file(tmp_path, "")) == {}
    assert MODULE._load_yaml_simple(
        metadata_file(
            tmp_path,
            'task_name: ""\ncategory:\n',
        )
    ) == {"task_name": "", "category": ""}


def test_scalar_alias_and_duplicate_key_follow_yaml(tmp_path: Path) -> None:
    result = MODULE._load_yaml_simple(
        metadata_file(
            tmp_path,
            'name: &name "Mail: review"\ntask_name: *name\n'
            "difficulty: easy\ndifficulty: hard\n",
        )
    )
    assert result["task_name"] == "Mail: review"
    assert result["difficulty"] == "hard"


def test_names_mode_still_returns_directory_names(tmp_path: Path) -> None:
    directory = tmp_path / "tasks/exact-folder"
    directory.mkdir(parents=True)
    metadata_file(directory, 'task_id: "T001"\ndifficulty: "hard" # comment\n')
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--tasks-dir",
            str(directory.parent),
            "--difficulty",
            "hard",
            "--format",
            "names",
        ],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
        timeout=20,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.splitlines() == ["exact-folder"]


def test_non_mapping_metadata_has_a_file_context(tmp_path: Path) -> None:
    path = metadata_file(tmp_path, "- invalid root\n")
    with pytest.raises(ValueError, match="task.yaml"):
        MODULE._load_yaml_simple(path)
