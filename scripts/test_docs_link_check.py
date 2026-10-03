#!/usr/bin/env python3
"""Regression tests for the file scope and link rules of docs-link-check."""

from __future__ import annotations

import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "docs-link-check.py"


def load_module():
    # The script filename is dashed, so it must be imported by location.
    spec = importlib.util.spec_from_file_location("docs_link_check", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class FileScopeContractTest(unittest.TestCase):
    """Pin the git ls-files pathspec contract in an isolated repository."""

    def setUp(self) -> None:
        temporary_directory = tempfile.TemporaryDirectory()
        self.addCleanup(temporary_directory.cleanup)
        self.root = Path(temporary_directory.name)
        subprocess.run(
            ["git", "init", "-q", str(self.root)],
            check=True, capture_output=True,
        )
        in_scope = [
            "docs/guide/index.md",
            "README.md",
            "src/foo/README.md",
            "src/foo/README_zh.md",
            "src/foo/python/bar/README.md",
            "deprecated/foo/README.md",
            "deprecated/foo/README_zh.md",
            "distribution/foo/README.md",
            "distribution/foo/README_zh.md",
        ]
        out_of_scope = [
            "src/foo/adapters/README.md",
            "deprecated/foo/guide/README.md",
            "distribution/foo/docs/README.md",
            "src/foo/internal/design.md",
        ]
        for name in in_scope + out_of_scope:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("stub\n", encoding="utf-8")
        subprocess.run(
            ["git", "add", "-A"],
            cwd=self.root, check=True, capture_output=True,
        )
        self.in_scope = in_scope
        self.out_of_scope = out_of_scope
        self.module = load_module()

    def selected(self) -> set[str]:
        return {
            p.relative_to(self.root).as_posix()
            for p in self.module.files_to_check(self.root)
        }

    def test_reader_facing_paths_are_selected(self):
        selected = self.selected()
        for name in self.in_scope:
            with self.subTest(name=name):
                self.assertIn(name, selected)

    def test_deep_in_tree_paths_stay_excluded(self):
        selected = self.selected()
        for name in self.out_of_scope:
            with self.subTest(name=name):
                self.assertNotIn(name, selected)


class FileScopeLiveTest(unittest.TestCase):
    """The relocated component READMEs are part of the gate."""

    def test_relocated_component_readmes_are_in_scope(self):
        module = load_module()
        root = module.repo_root()
        selected = {
            path.relative_to(root).as_posix()
            for path in module.files_to_check(root)
        }
        for path in (
            "deprecated/copilot-shell/README.md",
            "deprecated/copilot-shell/README_zh.md",
            "distribution/anolisa/README.md",
            "distribution/anolisa/README_zh.md",
        ):
            with self.subTest(path=path):
                self.assertIn(path, selected)


class CheckFileTest(unittest.TestCase):
    """Basic link-resolution behavior against fixture markdown."""

    def setUp(self) -> None:
        temporary_directory = tempfile.TemporaryDirectory()
        self.addCleanup(temporary_directory.cleanup)
        self.root = Path(temporary_directory.name)
        (self.root / "img").mkdir()
        (self.root / "img" / "logo.png").write_bytes(b"")
        (self.root / "target.md").write_text("# Target\n", encoding="utf-8")
        self.module = load_module()

    def write_page(self, text: str) -> Path:
        page = self.root / "page.md"
        page.write_text(text, encoding="utf-8")
        return page

    def test_broken_relative_link_is_flagged(self):
        page = self.write_page("see the [guide](missing.md)\n")
        errors = self.module.check_file(page, self.root)
        self.assertEqual(len(errors), 1)
        self.assertIn("missing.md", errors[0])
        self.assertIn("page.md", errors[0])

    def test_broken_image_is_flagged(self):
        page = self.write_page("![logo](img/missing.png)\n")
        errors = self.module.check_file(page, self.root)
        self.assertEqual(len(errors), 1)
        self.assertIn("img/missing.png", errors[0])

    def test_resolving_targets_stay_clean(self):
        page = self.write_page(
            "[a](target.md) [b](img/logo.png) [c](target.md#section \"title\") "
            "[d](https://example.com) [e](mailto:someone@example.com) [f](#anchor)\n"
        )
        self.assertEqual(self.module.check_file(page, self.root), [])

    def test_links_inside_fenced_code_blocks_are_ignored(self):
        page = self.write_page(
            "```\n[broken](missing.md)\n```\n\nsee the [guide](target.md)\n"
        )
        self.assertEqual(self.module.check_file(page, self.root), [])


if __name__ == "__main__":
    unittest.main()
