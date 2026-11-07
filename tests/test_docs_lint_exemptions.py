"""Exercise the complete docs lint CLI with actual Git and temporary files."""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SOURCE_ROOT = Path(
    os.environ.get("ANOLISA_DOCS_LINT_SOURCE_ROOT", str(Path(__file__).resolve().parents[1]))
)
BASH = os.environ.get("ANOLISA_BASH") or shutil.which("bash")
GIT = shutil.which("git")


class DocsLintExemptionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.assertIsNotNone(BASH, "Main must supply a real Bash executable")
        self.assertIsNotNone(GIT, "Main must supply actual Git, not a stand-in")
        self.temporary = tempfile.TemporaryDirectory(prefix="docs-lint-exemptions-")
        self.addCleanup(self.temporary.cleanup)
        self.repo = Path(self.temporary.name)
        self.git("init", "--quiet")
        (self.repo / "scripts").mkdir()
        shutil.copyfile(SOURCE_ROOT / "scripts/docs-lint.sh", self.repo / "scripts/docs-lint.sh")
        (self.repo / ".github").mkdir()
        for tree in ("docs/user-guide", "docs/developer-guide", "contribute"):
            for language in ("en", "zh"):
                self.write(tree + "/" + language + "/index.md", "# Matched guide\n")

    def git(self, *arguments: str) -> None:
        result = subprocess.run(
            [GIT, "-C", str(self.repo), *arguments],
            capture_output=True,
            check=False,
            text=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def write(self, relative: str, text: str) -> None:
        destination = self.repo / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(text, encoding="utf-8")

    def lint(self, exemptions: str) -> subprocess.CompletedProcess:
        self.write(".github/docs-lint-exemptions.txt", exemptions)
        self.git("add", "--all")
        return subprocess.run(
            [BASH, "scripts/docs-lint.sh"],
            cwd=self.repo,
            capture_output=True,
            check=False,
            text=True,
            encoding="utf-8",
            timeout=30,
        )

    def assert_passes(self, result: subprocess.CompletedProcess) -> None:
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("en/zh tree parity OK", result.stdout)

    def test_documented_relative_filename_exempts_pending_translation(self) -> None:
        self.write("docs/user-guide/en/pending.md", "# Pending translation\n")
        self.assert_passes(self.lint("pending.md\n"))

    def test_nested_literal_paths_apply_to_both_language_trees(self) -> None:
        self.write("docs/user-guide/en/topic/pending [1].md", "# English-only draft\n")
        self.write("docs/user-guide/zh/other/pending.md", "# Chinese-only draft\n")
        self.assert_passes(
            self.lint("# Pending translations\n\ntopic/pending [1].md\nother/pending.md\n")
        )

    def test_existing_dot_slash_exemption_still_works(self) -> None:
        self.write("docs/user-guide/en/pending.md", "# Existing exemption\n")
        self.assert_passes(self.lint("./pending.md\n"))

    def test_matched_trees_without_exemptions_still_pass(self) -> None:
        self.assert_passes(self.lint("# No pending translations\n\n"))

    def test_unlisted_prefix_sibling_still_fails(self) -> None:
        self.write("docs/user-guide/en/pending.md", "# Existing exemption\n")
        self.write("docs/user-guide/en/pending-old.md", "# Must still be translated\n")
        result = self.lint("./pending.md\n")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("trees are not mirrored", result.stdout)
        self.assertIn("pending-old.md", result.stdout)

    def test_exemption_does_not_disable_naming_validation(self) -> None:
        for language in ("en", "zh"):
            self.write("docs/user-guide/" + language + "/wrong_CN.md", "# Invalid naming\n")
        result = self.lint("./wrong_CN.md\n")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("Illegal Chinese doc naming", result.stdout)


if __name__ == "__main__":
    unittest.main()
