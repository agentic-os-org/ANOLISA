"""Characterize the two guide entry points' shared environment-readiness contract."""

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = ROOT / "src/os-skills/others/anolisa-guide/scripts"


def load_script(name):
    spec = importlib.util.spec_from_file_location("probe_contract_" + name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    previous_path = sys.path[:]
    try:
        sys.path.insert(0, str(SCRIPTS))
        spec.loader.exec_module(module)
    finally:
        sys.path[:] = previous_path
    return module


class GuideDependencyProbeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.modules = [load_script("setup_env"), load_script("check_docs")]

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.environment = Path(directory.name) / "guide environment"
        self.interpreter = self.environment / "bin" / "python"
        self.interpreter.parent.mkdir(parents=True)
        self.interpreter.touch()
        self.windows_interpreter = self.environment / "Scripts" / "python.exe"
        self.windows_interpreter.parent.mkdir()
        self.windows_interpreter.touch()

    def test_both_entry_points_probe_all_dependencies_with_same_deadline(self):
        for module in self.modules:
            with (
                self.subTest(entrypoint=module.__name__),
                mock.patch.object(module, "VENV_DIR", self.environment),
                mock.patch.object(subprocess, "run", return_value=mock.Mock(returncode=0)) as run,
            ):
                self.assertTrue(module.check_venv())
                command = run.call_args.args[0]
                self.assertEqual(
                    command,
                    [str(module.get_venv_python()), "-c", "import requests, bs4, markdownify"],
                )
                self.assertEqual(run.call_args.kwargs, {"capture_output": True, "timeout": 5})

    def test_missing_dependencies_are_not_ready(self):
        for module in self.modules:
            with (
                self.subTest(entrypoint=module.__name__),
                mock.patch.object(module, "VENV_DIR", self.environment),
                mock.patch.object(subprocess, "run", return_value=mock.Mock(returncode=1)),
            ):
                self.assertFalse(module.check_venv())

    def test_unavailable_interpreter_never_launches_a_process(self):
        self.interpreter.unlink()
        self.windows_interpreter.unlink()
        for module in self.modules:
            with (
                self.subTest(entrypoint=module.__name__),
                mock.patch.object(module, "VENV_DIR", self.environment),
                mock.patch.object(subprocess, "run") as run,
            ):
                self.assertFalse(module.check_venv())
                run.assert_not_called()

    def test_missing_environment_never_launches_a_process(self):
        for module in self.modules:
            with (
                self.subTest(entrypoint=module.__name__),
                mock.patch.object(module, "VENV_DIR", self.environment / "absent"),
                mock.patch.object(subprocess, "run") as run,
            ):
                self.assertFalse(module.check_venv())
                run.assert_not_called()

    def test_probe_errors_and_timeouts_are_not_ready(self):
        errors = [PermissionError("not executable"), subprocess.TimeoutExpired("python", 5)]
        for module in self.modules:
            for error in errors:
                with (
                    self.subTest(entrypoint=module.__name__, error=type(error).__name__),
                    mock.patch.object(module, "VENV_DIR", self.environment),
                    mock.patch.object(subprocess, "run", side_effect=error),
                ):
                    self.assertFalse(module.check_venv())

    def test_readiness_probe_does_not_create_or_install_anything(self):
        for module in self.modules:
            with (
                self.subTest(entrypoint=module.__name__),
                mock.patch.object(module, "VENV_DIR", self.environment),
                mock.patch.object(subprocess, "run", return_value=mock.Mock(returncode=0)) as run,
            ):
                original_entries = sorted(self.environment.rglob("*"))
                self.assertTrue(module.check_venv())
                self.assertEqual(sorted(self.environment.rglob("*")), original_entries)
                self.assertEqual(run.call_count, 1)
                self.assertNotIn("pip", run.call_args.args[0])


if __name__ == "__main__":
    unittest.main()
