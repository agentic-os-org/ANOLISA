#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Both guide utilities must use the interpreter created by the host venv layout."""

import importlib.util
import subprocess
import sys
import tempfile
import unittest
import venv
from pathlib import Path
from types import ModuleType
from unittest import mock

SCRIPTS = Path(__file__).resolve().parents[2] / "src/os-skills/others/anolisa-guide/scripts"


def load_script(name: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    with mock.patch.object(sys, "path", [str(SCRIPTS), *sys.path]):
        spec.loader.exec_module(module)
    return module


setup_env = load_script("setup_env")
check_docs = load_script("check_docs")


class GuideVenvTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.venv_dir = Path(self.temp_dir.name) / ".venv"
        (self.venv_dir / "Scripts").mkdir(parents=True)
        self.windows_python = self.venv_dir / "Scripts/python.exe"
        self.windows_python.touch()
        for module in (setup_env, check_docs):
            patcher = mock.patch.object(module, "VENV_DIR", self.venv_dir)
            patcher.start()
            self.addCleanup(patcher.stop)

    def test_windows_layout_selected_by_both_utilities(self) -> None:
        with mock.patch.object(sys, "platform", "win32"):
            for module in (setup_env, check_docs):
                self.assertEqual(module.get_venv_python(), self.windows_python)

    def test_posix_layout_selected_by_both_utilities(self) -> None:
        with mock.patch.object(sys, "platform", "linux"):
            for module in (setup_env, check_docs):
                self.assertEqual(module.get_venv_python(), self.venv_dir / "bin/python")

    def test_setup_readiness_checks_windows_interpreter(self) -> None:
        with mock.patch.object(sys, "platform", "win32"), mock.patch.object(
            subprocess, "run", return_value=subprocess.CompletedProcess([], 0)
        ) as run:
            self.assertTrue(setup_env.check_venv())
            self.assertEqual(run.call_args.args[0][0], str(self.windows_python))

    def test_selector_readiness_checks_windows_interpreter(self) -> None:
        with mock.patch.object(sys, "platform", "win32"), mock.patch.object(
            subprocess, "run", return_value=subprocess.CompletedProcess([], 0)
        ) as run:
            self.assertTrue(check_docs.check_venv())
            self.assertEqual(run.call_args.args[0][0], str(self.windows_python))

    def test_dependencies_installed_using_the_selected_python_module(self) -> None:
        with mock.patch.object(sys, "platform", "win32"), mock.patch.object(
            subprocess, "run", return_value=subprocess.CompletedProcess([], 0)
        ) as run:
            self.assertTrue(setup_env.install_dependencies())
            self.assertEqual(
                run.call_args.args[0][:4], [str(self.windows_python), "-m", "pip", "install"]
            )
            self.assertEqual(run.call_args.args[0][-3:], setup_env.DEPENDENCIES)

    def test_real_temporary_venv_interpreter_is_found(self) -> None:
        actual_dir = Path(self.temp_dir.name) / "actual-venv"
        venv.EnvBuilder(with_pip=False).create(actual_dir)
        for module in (setup_env, check_docs):
            with mock.patch.object(module, "VENV_DIR", actual_dir):
                self.assertTrue(module.get_venv_python().is_file())


if __name__ == "__main__":
    unittest.main()
