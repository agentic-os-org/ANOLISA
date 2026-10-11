"""Tracked Unicode Markdown paths must participate in the link gate."""

import importlib.util
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/docs-link-check.py"
SPEC = importlib.util.spec_from_file_location("docs_link_index", SCRIPT)
CHECKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)


class IndexPathTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name) / "仓库"
        self.root.mkdir()
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        (self.root / "docs").mkdir()

    def assert_checked(self, filename: str) -> None:
        document = self.root / "docs" / filename
        document.write_text("[missing](absent.md)\n", encoding="utf-8")
        subprocess.run(["git", "add", "--", str(document)], cwd=self.root, check=True)
        files = CHECKER.files_to_check(self.root)
        self.assertIn(document, files)
        self.assertEqual(len(CHECKER.check_file(document, self.root)), 1)

    def test_unicode_filename_is_checked_with_default_git_quoting(self) -> None:
        self.assert_checked("读者指南.md")

    def test_spaces_do_not_split_a_filename(self) -> None:
        self.assert_checked("reader guide.md")

    def test_unicode_repository_root_is_decoded_as_utf8(self) -> None:
        previous = Path.cwd()
        try:
            os.chdir(self.root)
            self.assertEqual(CHECKER.repo_root().resolve(), self.root.resolve())
        finally:
            os.chdir(previous)

    @unittest.skipIf(os.name == "nt", "Windows forbids control characters in filenames")
    def test_control_characters_do_not_split_index_records(self) -> None:
        self.assert_checked("reader\tguide\nnotes.md")


if __name__ == "__main__":
    unittest.main()
