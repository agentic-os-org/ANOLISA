"""Malformed JSON metadata reports validation errors with file context."""

import importlib.util
import io
import json
import tempfile
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/check-component-versions.py"
SPEC = importlib.util.spec_from_file_location("component_versions_json", SCRIPT)
CHECKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)
LOCK_PATH = "src/agent-memory/adapters/agent-memory/openclaw/package-lock.json"


class JsonContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.root_patch = patch.object(CHECKER, "ROOT", self.root)
        self.root_patch.start()
        self.addCleanup(self.root_patch.stop)

    def write(self, name: str, data: object) -> None:
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(data), encoding="utf-8")

    def test_version_file_requires_an_object(self) -> None:
        for value in (None, [], "text", 42, True):
            with self.subTest(value=value):
                self.write("package.json", value)
                with self.assertRaisesRegex(ValueError, "package.json.*object"):
                    CHECKER.read_json_version("package.json")

    def test_lock_file_requires_an_object(self) -> None:
        for value in (None, [], "text", 42):
            with self.subTest(value=value):
                self.write(LOCK_PATH, value)
                with self.assertRaisesRegex(ValueError, "package-lock.json.*object"):
                    CHECKER.check_agent_memory_lock([], "1.0.0")

    def test_optional_packages_and_root_entries_require_objects(self) -> None:
        for packages in (None, [], {"": []}, {"": None}, {"": "text"}):
            with self.subTest(packages=packages):
                self.write(LOCK_PATH, {"version": "1.0.0", "packages": packages})
                with self.assertRaisesRegex(ValueError, "package-lock.json.*object"):
                    CHECKER.check_agent_memory_lock([], "1.0.0")

    def test_current_and_older_locks_keep_their_version_checks(self) -> None:
        for data in (
            {"version": "1.0.0"},
            {"version": "1.0.0", "packages": {"": {"version": "1.0.0"}}},
        ):
            with self.subTest(data=data):
                self.write(LOCK_PATH, data)
                errors = []
                CHECKER.check_agent_memory_lock(errors, "1.0.0")
                self.assertEqual(errors, [])
        self.write(LOCK_PATH, {"version": "2.0.0", "packages": {"": {"version": "3.0.0"}}})
        errors = []
        CHECKER.check_agent_memory_lock(errors, "1.0.0")
        self.assertEqual(len(errors), 2)

    def test_gate_returns_a_file_attributed_error_for_invalid_metadata(self) -> None:
        self.write("source.json", {"version": "1.0.0"})
        self.write("target.json", [])
        output = io.StringIO()
        with (
            patch.object(CHECKER, "TOML_CONTRACTS", (("source.json", "target.json"),)),
            redirect_stderr(output),
        ):
            self.assertEqual(CHECKER.main(), 1)
        self.assertIn("target.json: expected a JSON object", output.getvalue())
        self.assertNotIn("Traceback", output.getvalue())


if __name__ == "__main__":
    unittest.main()
