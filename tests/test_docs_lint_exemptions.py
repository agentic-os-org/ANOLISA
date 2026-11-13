#!/usr/bin/env python3
"""Exercise parity exemptions using independent temporary Git repositories."""

import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SOURCE = Path(__file__).resolve().parents[1] / "scripts/docs-lint.sh"
BASH = shutil.which("bash")
if Path("C:/Program Files/Git/bin/bash.exe").exists():
    BASH = "C:/Program Files/Git/bin/bash.exe"


@unittest.skipUnless(BASH and shutil.which("git"), "Bash and Git are required")
class ExemptionTests(unittest.TestCase):
    def run_lint(self, exemptions: str, missing: tuple[str, ...] = ("nested/pending.md",)):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for language in ("en", "zh"):
                (root / "docs/user-guide" / language).mkdir(parents=True)
            for path in missing:
                target = root / "docs/user-guide/en" / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text("Pending translation\n", encoding="utf-8")
            (root / ".github").mkdir()
            (root / ".github/docs-lint-exemptions.txt").write_bytes(exemptions.encode("utf-8"))
            script = root / "lint.sh"
            script.write_bytes(SOURCE.read_bytes().replace(b"\r\n", b"\n"))
            subprocess.run(["git", "init", "-q", str(root)], check=True, capture_output=True)
            subprocess.run(["git", "add", "."], cwd=root, check=True, capture_output=True)
            return subprocess.run(
                [BASH, script.as_posix()], cwd=root, capture_output=True,
                text=True, encoding="utf-8", check=False
            )

    def test_documented_relative_path_exempts_pending_translation(self):
        result = self.run_lint("nested/pending.md\n")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_legacy_dot_prefix_remains_supported(self):
        result = self.run_lint("./nested/pending.md\n")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_crlf_exemptions_use_the_same_path_convention(self):
        result = self.run_lint("# pending translation\r\nnested/pending.md\r\n\r\n")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_exemption_does_not_hide_unrelated_missing_translation(self):
        result = self.run_lint("nested/pending.md\n", ("nested/pending.md", "other.md"))
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("other.md", result.stdout)
        self.assertNotIn("pending.md", result.stdout)

    def test_comment_only_exemptions_preserve_parity_failure(self):
        result = self.run_lint("# no pending translations\n\n")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("pending.md", result.stdout)


if __name__ == "__main__":
    unittest.main()
