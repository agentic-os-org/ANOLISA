"""Inventory tag selection follows exact YAML list membership."""

import importlib.util
import sys
from pathlib import Path

import pytest


@pytest.fixture
def inventory(monkeypatch: pytest.MonkeyPatch, tmp_path: Path):
    path = Path(__file__).parents[1] / "scripts/list_tasks.py"
    spec = importlib.util.spec_from_file_location("inventory_tags_test", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    tasks = tmp_path / "tasks"
    tasks.mkdir()
    cases = {
        "C01": ("hard", "tags: [user_agent, general]"),
        "T001": ("hard", "tags:\n  - general\n  - email"),
        "T002": ("easy", "tags: [general_extra]"),
        "M001": ("hard", "tags: []"),
        "T003": ("easy", "tags: ['General']"),
    }
    for name, (difficulty, tags) in cases.items():
        directory = tasks / name
        directory.mkdir()
        (directory / "task.yaml").write_text(
            f"task_id: {name}\ndifficulty: {difficulty}\n{tags}\n", encoding="utf-8"
        )
    monkeypatch.setattr(module, "TASKS_DIR", tasks)
    return module, tasks


@pytest.mark.parametrize(
    "tag,expected",
    [
        ("general", ["C01", "T001"]),
        ("user_agent", ["C01"]),
        ("General", ["T003"]),
        ("gener", []),
        ("missing", []),
    ],
)
def test_exact_tag_membership_supports_inline_and_block_lists(inventory, tag, expected):
    module, _ = inventory
    assert [row["task_id"] for row in module.scan_tasks(tag_filter=tag)] == expected


def test_tag_combines_with_existing_prefix_and_difficulty(inventory):
    module, _ = inventory
    assert [
        row["task_id"]
        for row in module.scan_tasks(
            prefix_filter="T", difficulty_filter="hard", tag_filter="general"
        )
    ] == ["T001"]


@pytest.mark.parametrize("tag", ["general", "missing"])
def test_cli_filters_existing_grouped_view(inventory, monkeypatch, capsys, tag):
    module, _ = inventory
    monkeypatch.setattr(sys, "argv", ["list_tasks.py", "--tag", tag])
    module.main()
    output = capsys.readouterr().out
    if tag == "general":
        assert "C01" in output and "T001" in output
        assert "T002" not in output
    else:
        assert "No tasks found" in output


@pytest.mark.parametrize("tag", ["", "   "])
def test_cli_rejects_empty_tag(inventory, monkeypatch, capsys, tag):
    module, _ = inventory
    monkeypatch.setattr(sys, "argv", ["list_tasks.py", "--tag", tag])
    with pytest.raises(SystemExit) as raised:
        module.main()
    assert raised.value.code == 2
    assert "tag must not be empty" in capsys.readouterr().err


def test_default_inventory_preserves_all_metadata_and_order(inventory):
    module, _ = inventory
    rows = module.scan_tasks()
    assert [row["task_id"] for row in rows] == ["C01", "M001", "T001", "T002", "T003"]
    assert rows[0]["tags"] == "[user_agent, general]"


def test_default_inventory_does_not_require_yaml_dependency(inventory, monkeypatch):
    module, _ = inventory
    monkeypatch.setitem(sys.modules, "yaml", None)
    assert len(module.scan_tasks()) == 5


def test_missing_tag_dependency_has_actionable_usage_error(
    inventory, monkeypatch, capsys
):
    module, _ = inventory
    monkeypatch.setitem(sys.modules, "yaml", None)
    monkeypatch.setattr(sys, "argv", ["list_tasks.py", "--tag", "general"])
    with pytest.raises(SystemExit) as raised:
        module.main()
    assert raised.value.code == 2
    assert "requires PyYAML" in capsys.readouterr().err


@pytest.mark.parametrize(
    "tags,expected",
    [("general", False), ("{general: true}", False), ("[general, 2]", True)],
)
def test_tag_filter_uses_only_string_members_of_yaml_lists(inventory, tags, expected):
    module, tasks = inventory
    source = tasks / "custom.yaml"
    source.write_text("tags: " + tags + "\n", encoding="utf-8")
    assert module._matches_tag(source, "general") is expected
