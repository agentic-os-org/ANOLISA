"""Full link-check CLI over a genuine temporary Git repository, stdlib only."""

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SOURCE_ROOT = Path(
    os.environ.get("ANOLISA_SOURCE_ROOT", Path(__file__).resolve().parents[1])
)
SCRIPT = SOURCE_ROOT / "scripts/docs-link-check.py"


class LinkTitleCliTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.environment = dict(os.environ, PYTHONIOENCODING="utf-8")
        subprocess.run(
            ["git", "init", "--quiet", str(self.root)],
            check=True,
            capture_output=True,
            env=self.environment,
            timeout=30,
        )
        (self.root / "guide.md").write_text("# Guide\n", encoding="utf-8")

    def check(self, markdown):
        (self.root / "README.md").write_text(markdown + "\n", encoding="utf-8")
        # The exact dependency head enumerates tracked files, not untracked ones.
        subprocess.run(
            ["git", "add", "README.md", "guide.md"],
            cwd=self.root,
            env=self.environment,
            capture_output=True,
            check=True,
            timeout=30,
        )
        return subprocess.run(
            [sys.executable, str(SCRIPT)],
            cwd=self.root,
            env=self.environment,
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=False,
            timeout=30,
        )

    def assert_missing(self, result, target):
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("1 broken relative link(s)", result.stdout)
        self.assertIn("README.md: broken link -> " + target, result.stdout)
        self.assertNotIn("Traceback", result.stderr)

    def test_single_quoted_title_keeps_the_missing_destination_checked(self):
        result = self.check("[guide](missing.md 'display title')")
        self.assert_missing(result, "missing.md")

    def test_parenthesized_image_title_keeps_the_missing_destination_checked(self):
        result = self.check("![diagram](missing.png (display title))")
        self.assert_missing(result, "missing.png")

    def test_link_like_text_inside_titles_is_not_a_separate_destination(self):
        for markdown in (
            "[guide](guide.md 'see [example](not-a-target.md)')",
            r'[guide](guide.md "see \"[example](not-a-target.md)\"")',
        ):
            with self.subTest(markdown=markdown):
                result = self.check(markdown)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("All relative links resolve", result.stdout)

    def test_plain_missing_destination_control(self):
        self.assert_missing(self.check("[guide](missing.md)"), "missing.md")

    def test_double_quoted_missing_destination_control(self):
        self.assert_missing(
            self.check('[guide](missing.md "display title")'), "missing.md"
        )

    def test_existing_destination_and_fragment_controls(self):
        result = self.check(
            '[guide](guide.md) [section](guide.md#intro "display title") [local](#intro)'
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("All relative links resolve", result.stdout)


if __name__ == "__main__":
    unittest.main()
