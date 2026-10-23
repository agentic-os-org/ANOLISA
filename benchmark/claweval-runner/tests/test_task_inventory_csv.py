"""Offline inventory CSV is a stable machine stream, including empty selections."""

import csv
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SOURCE = Path(__file__).parents[1] / "scripts/list_tasks.py"
FIELDS = ["task_id", "task_name", "difficulty", "prefix", "category", "tags"]


class TaskInventoryCsvTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.script = self.root / "scripts/list_tasks.py"
        self.script.parent.mkdir()
        self.script.write_bytes(SOURCE.read_bytes())
        self.tasks = self.root / "claw-eval/tasks"
        self.tasks.mkdir(parents=True)
        for directory, fields in (
            ("T030", {"task_id": "T03", "difficulty": "hard", "task_name": "Last"}),
            ("C020", {"task_id": "C02", "difficulty": "easy", "task_name": "Middle"}),
            ("T010", {"task_id": "T01", "difficulty": "hard", "task_name": '收入, "核对"'}),
        ):
            task = self.tasks / directory
            task.mkdir()
            text = "\n".join(f"{key}: {value}" for key, value in fields.items())
            text += "\ncategory: finance\ntags: [general, review]\n"
            (task / "task.yaml").write_text(text, encoding="utf-8")

    def run_cli(self, *arguments, encoding="utf-8"):
        return subprocess.run(
            [sys.executable, str(self.script), *arguments],
            cwd=self.root.parent,
            env={**os.environ, "PYTHONIOENCODING": encoding, "PYTHONUTF8": "0"},
            capture_output=True,
            timeout=15,
        )

    def records(self, result):
        self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", errors="replace"))
        text = result.stdout.decode("utf-8")
        self.assertFalse(text.startswith("\ufeff"))
        stream = io.StringIO(text, newline="")
        reader = csv.DictReader(stream)
        self.assertEqual(reader.fieldnames, FIELDS)
        return list(reader)

    def test_actual_cli_has_fixed_columns_sorted_rows_and_no_display_text(self):
        result = self.run_cli("--format", "csv")
        records = self.records(result)
        self.assertEqual([record["task_id"] for record in records], ["C02", "T01", "T03"])
        self.assertEqual(records[0]["prefix"], "C")
        self.assertEqual(records[0]["category"], "finance")
        self.assertEqual(records[0]["tags"], "[general, review]")
        self.assertNotIn(b"Grand total", result.stdout)
        self.assertEqual(result.stdout, self.run_cli("--format", "csv").stdout)

    def test_unicode_commas_and_quotes_round_trip_with_ascii_console(self):
        result = self.run_cli("--format", "csv", encoding="ascii")
        records = self.records(result)
        self.assertEqual(records[1]["task_name"], '收入, "核对"')
        self.assertIn('"收入, ""核对"""'.encode(), result.stdout)

    def test_prefix_and_difficulty_filters_share_existing_selection(self):
        selected = self.records(
            self.run_cli("--format", "csv", "--prefix", "T", "--difficulty", "hard")
        )
        self.assertEqual([record["task_id"] for record in selected], ["T01", "T03"])

    def test_empty_filtered_selection_is_header_only(self):
        result = self.run_cli("--format", "csv", "--difficulty", "expert")
        self.assertEqual(self.records(result), [])
        self.assertEqual(result.stdout, (",".join(FIELDS) + "\r\n").encode())

    def test_empty_task_root_is_header_only(self):
        for path in self.tasks.iterdir():
            (path / "task.yaml").unlink()
        result = self.run_cli("--format", "csv")
        self.assertEqual(self.records(result), [])

    def test_newline_values_round_trip_through_real_cli_dispatch_and_writer(self):
        records = [
            {
                "task_id": "T01",
                "task_name": 'first\nsecond, "quoted"\r\n第三行\rlast',
                "difficulty": "hard",
                "prefix": "T",
                "category": "",
                "tags": "",
            }
        ]
        payload = self.root / "captured-metadata.json"
        payload.write_text(json.dumps(records), encoding="utf-8")
        bootstrap = (
            "import importlib.util,json,sys;"
            "spec=importlib.util.spec_from_file_location('actual_inventory',sys.argv[1]);"
            "module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);"
            "rows=json.loads(open(sys.argv[2],encoding='utf-8').read());"
            "module.scan_tasks=lambda **kwargs:rows;"
            "sys.argv=[sys.argv[1],'--format','csv'];module.main()"
        )
        result = subprocess.run(
            [sys.executable, "-c", bootstrap, str(self.script), str(payload)],
            env={**os.environ, "PYTHONIOENCODING": "ascii", "PYTHONUTF8": "0"},
            capture_output=True,
            timeout=15,
        )
        self.assertEqual(self.records(result), records)

    def test_export_does_not_change_metadata_files(self):
        original = {path: path.read_bytes() for path in self.tasks.rglob("task.yaml")}
        self.records(self.run_cli("--format", "csv"))
        self.assertEqual({path: path.read_bytes() for path in original}, original)

    def test_default_and_explicit_grouped_output_are_identical(self):
        original = self.run_cli()
        explicit = self.run_cli("--format", "grouped")
        self.assertEqual(original.returncode, 0)
        self.assertEqual(explicit.returncode, 0)
        self.assertEqual(original.stdout, explicit.stdout)
        self.assertIn(b"Grand total: 3 tasks", original.stdout)

    def test_default_grouped_mode_retains_existing_summary(self):
        result = self.run_cli()
        self.assertEqual(result.returncode, 0)
        self.assertIn(b"Grand total: 3 tasks", result.stdout)
        self.assertIn(b"Prefix: T", result.stdout)
        self.assertIn(b"Prefix: C", result.stdout)

    def test_invalid_format_is_usage_error_before_task_scan(self):
        self.tasks.rename(self.root / "unavailable")
        result = self.run_cli("--format", "invalid")
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, b"")
        self.assertIn(b"invalid choice", result.stderr)


if __name__ == "__main__":
    unittest.main()
