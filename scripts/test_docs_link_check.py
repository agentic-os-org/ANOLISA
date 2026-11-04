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

    def test_reference_forms_are_checked_like_inline_links(self):
        doc = self.root / "docs" / "source.md"
        cases = [
            ("[Install][guide]\n\n[guide]: missing-guide.md\n", "missing-guide.md"),
            ("[Install][]\n\n[Install]: missing-collapsed.md\n", "missing-collapsed.md"),
            ("[Install]\n\n[Install]: missing-shortcut.md\n", "missing-shortcut.md"),
            ("![Diagram][img]\n\n[img]: missing-image.png\n", "missing-image.png"),
            # A target wrapped in angle brackets may contain spaces, and must
            # still be resolved rather than skipped as unparseable.
            ("[Plan][spaced]\n\n[spaced]: <missing plan.md>\n", "missing plan.md"),
            ("[Plan][titled]\n\n[titled]: <missing plan.md> \"Plan\"\n", "missing plan.md"),
        ]
        for text, target in cases:
            with self.subTest(target=target):
                doc.write_text(text, encoding="utf-8")
                result = self.check_links()
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn(target, result.stdout)

    def test_valid_reference_targets_pass(self):
        (self.root / "docs" / "guide.md").write_text("# Guide\n", encoding="utf-8")
        (self.root / "docs" / "diagram.png").write_bytes(b"\x89PNG")
        (self.root / "docs" / "spaced guide.md").write_text("# Spaced\n", encoding="utf-8")
        (self.root / "docs" / "source.md").write_text(
            "[Full][guide] [Collapsed][] [Shortcut] ![Image][img]\n\n"
            "[Collapsed]: guide.md#usage\n"
            "[Shortcut]: ./guide.md\n"
            "[guide]: guide.md\n"
            "[img]: diagram.png\n"
            "[spaced]: <spaced guide.md> \"Spaced guide\"\n",
            encoding="utf-8",
        )
        result = self.check_links()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_unused_reference_definition_is_not_an_error(self):
        (self.root / "docs" / "source.md").write_text(
            "# Title\n\n[unused]: missing-guide.md\n", encoding="utf-8"
        )
        result = self.check_links()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_a_shared_definition_is_reported_once(self):
        (self.root / "docs" / "source.md").write_text(
            "[One][guide] and [Two][guide]\n\n[guide]: missing-guide.md\n", encoding="utf-8"
        )
        result = self.check_links()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(result.stdout.count("missing-guide.md"), 1, result.stdout)

    def test_external_and_anchor_reference_targets_are_skipped(self):
        (self.root / "docs" / "source.md").write_text(
            "[Web][site] and [Anchor][part]\n\n"
            "[site]: https://example.com/guide\n"
            "[part]: #section\n",
            encoding="utf-8",
        )
        result = self.check_links()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
