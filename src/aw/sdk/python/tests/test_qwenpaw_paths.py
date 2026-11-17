"""Verify installed Adapter acceleration before native loading and during plugin entry."""

import importlib.metadata
import os
from pathlib import Path
import sys
import tempfile
from types import ModuleType
import unittest
from unittest.mock import patch

from aw_sdk.qwenpaw import prepare_runtime
from aw_sdk._native.qwenpaw import aw_paths


class RuntimePaths(unittest.TestCase):
    def setUp(self) -> None:
        self.original = lambda path: os.path.normcase(os.path.realpath(str(path)))
        self.isolation = ModuleType("qwenpaw.plugins.module_isolation")
        self.isolation._norm = self.original
        self.plugins = ModuleType("qwenpaw.plugins")
        self.plugins.module_isolation = self.isolation
        self.modules = {
            "qwenpaw": ModuleType("qwenpaw"),
            "qwenpaw.plugins": self.plugins,
            "qwenpaw.plugins.module_isolation": self.isolation,
        }

    def test_prepare_precedes_native_loading_and_keeps_one_patch(self) -> None:
        with patch.dict(sys.modules, self.modules), patch.object(
            importlib.metadata, "version", return_value="2.1.1b3"
        ), patch("subprocess.Popen", side_effect=AssertionError("unexpected process")):
            self.assertTrue(prepare_runtime())
            installed = self.isolation._norm
            self.assertIsNot(installed, self.original)
            self.assertTrue(prepare_runtime())
            self.assertIs(self.isolation._norm, installed)
            self.assertIs(aw_paths.accelerate_paths("2.1.1b3"), installed)
            self.assertNotIn("qwenpaw.app", sys.modules)

    def test_optional_runtime_and_unavailable_seam_keep_native_behavior(self) -> None:
        with patch.dict(sys.modules, self.modules):
            with patch.object(
                importlib.metadata, "version", side_effect=importlib.metadata.PackageNotFoundError
            ):
                self.assertFalse(prepare_runtime())
            for version in ("2.1.1b2", "2.2.2.post1", "2.1.1b3+local"):
                with patch.object(importlib.metadata, "version", return_value=version):
                    self.assertFalse(prepare_runtime())
            self.assertIs(self.isolation._norm, self.original)
            with patch.object(importlib.metadata, "version", return_value="2.1.1b3"):
                with patch.dict(sys.modules, {"ctypes": None}):
                    self.assertFalse(prepare_runtime())
                self.assertIs(self.isolation._norm, self.original)
                modified = lambda path: str(path)
                self.isolation._norm = modified
                self.assertFalse(prepare_runtime())
                self.assertIs(self.isolation._norm, modified)
                self.isolation._norm = None
                self.assertFalse(prepare_runtime())

    def test_plugin_entry_installs_acceleration_before_importing_middleware(self) -> None:
        from aw_sdk import qwenpaw

        for public, module in (("AfterOnlyPlugin", "after_only"), ("QwenPawPlugin", "sdk")):
            native = ModuleType("aw_sdk._native.qwenpaw." + module)
            sentinel = object()
            setattr(native, public, sentinel)
            self.isolation._norm = self.original
            with patch.dict(sys.modules, {**self.modules, native.__name__: native}), patch.object(
                importlib.metadata, "version", return_value="2.1.1b3"
            ):
                self.assertIs(getattr(qwenpaw, public), sentinel)
                self.assertIsNot(self.isolation._norm, self.original)

    def test_native_paths_observe_symlink_changes_and_missing_path_fallback(self) -> None:
        with patch.dict(sys.modules, self.modules), patch.object(
            importlib.metadata, "version", return_value="2.1.1b3"
        ), tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "first").mkdir()
            (root / "second").mkdir()
            link = root / "link"
            link.symlink_to("first")
            prepare_runtime()
            self.assertEqual(self.isolation._norm(link), self.original(link))
            link.unlink()
            link.symlink_to("second")
            self.assertEqual(self.isolation._norm(link), self.original(link))
            self.assertEqual(
                self.isolation._norm(root / "missing/child"), self.original(root / "missing/child")
            )
            with self.assertRaises(ValueError):
                self.isolation._norm("bad\0path")


if __name__ == "__main__":
    unittest.main()
