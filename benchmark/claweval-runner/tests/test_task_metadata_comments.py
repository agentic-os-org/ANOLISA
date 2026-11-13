"""Run the actual inventory script on ordinary flat task YAML with comments."""

import importlib.util
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import ModuleType

SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "list_tasks.py"


def load_inventory() -> ModuleType:
    spec = importlib.util.spec_from_file_location("task_inventory_comments", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class TaskMetadataCommentTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.runner = Path(temporary.name) / "runner"
        self.script = self.runner / "scripts" / "list_tasks.py"
        self.script.parent.mkdir(parents=True)
        # Byte-copy the full real script, preserving its normal relative task-root layout.
        shutil.copyfile(SCRIPT, self.script)
        self.tasks = self.runner / "claw-eval" / "tasks"
        self.tasks.mkdir(parents=True)

    def write_task(self, text: str) -> Path:
        task = self.tasks / "T001_demo"
        task.mkdir()
        path = task / "task.yaml"
        path.write_text(text, encoding="utf-8")
        return path

    def run_inventory(self, *options: str) -> str:
        original_script = self.script.read_bytes()
        originals = {path: path.read_bytes() for path in self.tasks.rglob("task.yaml")}
        result = subprocess.run(
            [sys.executable, str(self.script), *options],
            cwd=self.runner,
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=30,
            check=False,
        )
        self.assertEqual(self.script.read_bytes(), original_script)
        for path, content in originals.items():
            self.assertEqual(path.read_bytes(), content)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def test_unquoted_difficulty_comment_does_not_hide_task(self) -> None:
        self.write_task(
            "task_id: T001_demo\ndifficulty: hard # ordinary metadata comment\n"
        )
        output = self.run_inventory("--prefix", "T", "--difficulty", "hard")
        self.assertIn("    T001_demo\n", output)
        self.assertIn("Grand total: 1 tasks", output)

    def test_quoted_metadata_comments_do_not_change_selection_or_id(self) -> None:
        self.write_task(
            'task_id: "T001_demo" # task identity\n'
            "difficulty: 'hard' # difficulty annotation\n"
        )
        output = self.run_inventory("--prefix", "T", "--difficulty", "hard")
        self.assertIn("    T001_demo\n", output)
        self.assertIn("Grand total: 1 tasks", output)
        self.assertNotIn("task identity", output)
        self.assertNotIn("difficulty annotation", output)

    def test_quoted_hash_and_trailing_comment_have_distinct_meanings(self) -> None:
        path = self.write_task(
            'task_name: "Task # one" # actual comment\ndifficulty: hard\n'
        )
        module = load_inventory()
        self.assertEqual(
            module._load_yaml_simple(path),
            {"task_name": "Task # one", "difficulty": "hard"},
        )

    def test_hash_characters_inside_values_remain_data(self) -> None:
        path = self.write_task(
            'task_name: "Task # one"\ncategory: section#one\ndifficulty: hard\n'
            'tags: ["tag # one", plain#two]\n'
        )
        module = load_inventory()
        self.assertEqual(
            module._load_yaml_simple(path),
            {
                "task_name": "Task # one",
                "category": "section#one",
                "difficulty": "hard",
                "tags": '["tag # one", plain#two]',
            },
        )

    def test_existing_plain_listing_and_filter_are_unchanged(self) -> None:
        self.write_task("task_id: T001_demo\ndifficulty: hard\n")
        output = self.run_inventory("--prefix", "T", "--difficulty", "hard")
        self.assertIn("    T001_demo\n", output)
        self.assertIn("Grand total: 1 tasks", output)


if __name__ == "__main__":
    unittest.main()
