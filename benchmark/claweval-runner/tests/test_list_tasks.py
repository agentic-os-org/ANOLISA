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

"""Tests for scripts/list_tasks.py machine-readable inventory export.

Covers (per issue acceptance criteria):
- grouped output stays the default and unchanged in shape
- custom --tasks-dir root selection
- combined prefix + difficulty filters
- task_id vs directory_name differences surfaced in JSON metadata
- Unicode metadata round-trips through JSON
- empty selections produce a valid [] array / empty names stream with no
  explanatory text contaminating machine output
- direct script use from another working directory
"""

import json
import subprocess
import sys
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

REPO_ROOT = Path(__file__).resolve().parent.parent
SCRIPT = REPO_ROOT / "scripts" / "list_tasks.py"
sys.path.insert(0, str(REPO_ROOT / "scripts"))

from list_tasks import scan_tasks  # noqa: E402


def _make_task(base: Path, dirname: str, yaml_text: str) -> None:
    task_dir = base / dirname
    task_dir.mkdir()
    (task_dir / "task.yaml").write_text(yaml_text, encoding="utf-8")


class TestScanTasks(unittest.TestCase):
    """Direct function-level checks with mocked task roots."""

    def test_custom_root_and_metadata_shape(self):
        with TemporaryDirectory() as tmp:
            base = Path(tmp)
            _make_task(
                base,
                "T001_alpha",
                "task_id: T-custom-id\ndifficulty: easy\n"
                "task_name: Alpha 任务\ncategory: io\ntags: smoke\n",
            )
            (base / "stray.txt").write_text("plain file, not a task dir")
            (base / "no_yaml_dir").mkdir()

            tasks = scan_tasks(tasks_dir=base)
            self.assertEqual(len(tasks), 1)
            entry = tasks[0]
            self.assertEqual(
                sorted(entry.keys()),
                sorted(
                    [
                        "task_id",
                        "directory_name",
                        "task_name",
                        "difficulty",
                        "prefix",
                        "category",
                        "tags",
                    ]
                ),
            )
            # task_id differs from directory name and both are exposed
            self.assertEqual(entry["task_id"], "T-custom-id")
            self.assertEqual(entry["directory_name"], "T001_alpha")
            # Unicode metadata survives verbatim
            self.assertEqual(entry["task_name"], "Alpha 任务")
            self.assertEqual(entry["prefix"], "T")

    def test_combined_filters(self):
        with TemporaryDirectory() as tmp:
            base = Path(tmp)
            _make_task(base, "T001_a", "task_id: T001\ndifficulty: easy\n")
            _make_task(base, "T002_b", "task_id: T002\ndifficulty: hard\n")
            _make_task(base, "M001_c", "task_id: M001\ndifficulty: hard\n")

            tasks = scan_tasks(tasks_dir=base, prefix_filter="T",
                               difficulty_filter="hard")
            self.assertEqual([t["directory_name"] for t in tasks], ["T002_b"])

    def test_empty_root_yields_empty_list(self):
        with TemporaryDirectory() as tmp:
            self.assertEqual(scan_tasks(tasks_dir=Path(tmp)), [])


class TestCliFormats(unittest.TestCase):
    """CLI-level checks proving machine output shape on stdout."""

    def _run(self, *args: str, cwd: Path | None = None) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            capture_output=True,
            text=True,
            cwd=str(cwd) if cwd else None,
        )

    def _make_root(self, base: Path) -> None:
        _make_task(
            base,
            "T001_alpha",
            "task_id: T-custom-id\ndifficulty: easy\ntask_name: Alpha 任务\n",
        )
        _make_task(base, "M001_beta", "task_id: M001\ndifficulty: hard\n")

    def test_default_format_is_grouped(self):
        with TemporaryDirectory() as tmp:
            base = Path(tmp)
            self._make_root(base)
            result = self._run("--tasks-dir", str(base))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("Prefix: T", result.stdout)
            self.assertIn("T-custom-id", result.stdout)
            self.assertIn("Grand total: 2 tasks", result.stdout)

    def test_json_format_shape(self):
        with TemporaryDirectory() as tmp:
            base = Path(tmp)
            self._make_root(base)
            result = self._run("--tasks-dir", str(base), "--format", "json")
            self.assertEqual(result.returncode, 0, result.stderr)
            data = json.loads(result.stdout)
            self.assertIsInstance(data, list)
            self.assertEqual(len(data), 2)
            by_dir = {item["directory_name"]: item for item in data}
            self.assertEqual(by_dir["T001_alpha"]["task_id"], "T-custom-id")
            self.assertEqual(by_dir["M001_beta"]["difficulty"], "hard")
            # Unicode metadata is not escaped to \\uXXXX
            self.assertIn("Alpha 任务", result.stdout)

    def test_names_format_one_per_line(self):
        with TemporaryDirectory() as tmp:
            base = Path(tmp)
            self._make_root(base)
            result = self._run("--tasks-dir", str(base), "--format", "names")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                result.stdout.splitlines(), ["M001_beta", "T001_alpha"]
            )
            self.assertEqual(result.stdout, "M001_beta\nT001_alpha\n")

    def test_names_format_respects_filters(self):
        with TemporaryDirectory() as tmp:
            base = Path(tmp)
            self._make_root(base)
            result = self._run(
                "--tasks-dir", str(base), "--format", "names", "--difficulty", "hard"
            )
            self.assertEqual(result.stdout, "M001_beta\n")

    def test_empty_selection_json_is_empty_array(self):
        with TemporaryDirectory() as tmp:
            base = Path(tmp)
            self._make_root(base)
            result = self._run(
                "--tasks-dir", str(base), "--format", "json", "--prefix", "C"
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout), [])
            self.assertEqual(result.stdout.strip(), "[]")

    def test_empty_selection_names_is_empty_stream(self):
        with TemporaryDirectory() as tmp:
            base = Path(tmp)
            self._make_root(base)
            result = self._run(
                "--tasks-dir", str(base), "--format", "names", "--prefix", "C"
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, "")

    def test_script_runs_from_another_working_directory(self):
        with TemporaryDirectory() as tmp_cwd, TemporaryDirectory() as tmp_tasks:
            base = Path(tmp_tasks)
            self._make_root(base)
            result = self._run(
                "--tasks-dir", str(base), "--format", "json", cwd=Path(tmp_cwd)
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(len(json.loads(result.stdout)), 2)

    def test_missing_tasks_dir_errors_on_stderr(self):
        result = self._run("--tasks-dir", "/nonexistent/tasks-root-xyz", "--format", "json")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ERROR: tasks directory not found", result.stderr)
        self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
