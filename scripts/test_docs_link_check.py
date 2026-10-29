"""Exercise Git filename discovery and Markdown URL targets through the CLI."""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

CHECKER = Path(__file__).with_name("docs-link-check.py")


class DocsLinkCheckTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        (self.root / "docs").mkdir()

    def check_links(self):
        return subprocess.run(
            [sys.executable, str(CHECKER)], cwd=self.root,
            capture_output=True, text=True, encoding="utf-8",
        )

    def test_tracked_and_untracked_quoted_filenames_are_checked(self):
        names = ["normal.md", "中文.md", 'quote".md', "tab\tname.md", "line\nname.md"]
        for tracked in (False, True):
            for name in names:
                with self.subTest(tracked=tracked, name=name):
                    doc = self.root / "docs" / name
                    doc.write_text("[broken](missing.md)\n", encoding="utf-8")
                    if tracked:
                        subprocess.run(["git", "add", "--", str(doc)], cwd=self.root, check=True)
                    result = self.check_links()
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    self.assertIn("missing.md", result.stdout)
                    doc.unlink()
                    if tracked:
                        subprocess.run(["git", "rm", "--cached", "--", str(doc)], cwd=self.root, check=True, capture_output=True)

    def test_percent_encoded_existing_targets(self):
        doc = self.root / "docs" / "source.md"
        for filename, target in [
            ("a b.md", "a%20b.md#section"),
            ("a#b.md", "a%23b.md"),
            ("中文.md", "%E4%B8%AD%E6%96%87.md"),
            ("100%.md", "100%25.md"),
        ]:
            with self.subTest(target=target):
                (self.root / "docs" / filename).write_text("# Target\n", encoding="utf-8")
                doc.write_text(f"[target]({target})\n", encoding="utf-8")
                result = self.check_links()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_missing_encoded_target_remains_an_error(self):
        (self.root / "docs" / "source.md").write_text("[missing](missing%20file.md)\n", encoding="utf-8")
        result = self.check_links()
        self.assertEqual(result.returncode, 1)
        self.assertIn("missing%20file.md", result.stdout)

    def test_deleted_tracked_files_are_still_skipped(self):
        doc = self.root / "docs" / "deleted.md"
        doc.write_text("[broken](missing.md)\n", encoding="utf-8")
        subprocess.run(["git", "add", "--", str(doc)], cwd=self.root, check=True)
        doc.unlink()
        self.assertEqual(self.check_links().returncode, 0)


if __name__ == "__main__":
    unittest.main()
