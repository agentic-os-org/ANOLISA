"""The link-check CLI preserves exit status with restrictive stdout encodings."""

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/docs-link-check.py"


class LinkCheckEncodingTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.repository = Path(temporary.name)
        subprocess.run(["git", "init", "--quiet", str(self.repository)], check=True)

    def run_checker(
        self, encoding: str, broken: bool = False
    ) -> subprocess.CompletedProcess:
        text = "[missing](missing-\u2603.md)" if broken else "No relative links."
        (self.repository / "README.md").write_text(text, encoding="utf-8")
        subprocess.run(["git", "add", "README.md"], cwd=self.repository, check=True)
        environment = {
            **os.environ,
            "PYTHONUTF8": "0",
            "PYTHONIOENCODING": f"{encoding}:strict",
        }
        return subprocess.run(
            [sys.executable, str(SCRIPT)],
            cwd=self.repository,
            env=environment,
            capture_output=True,
            text=True,
            encoding=encoding,
            check=False,
        )

    def test_valid_links_succeed_with_restrictive_stdout(self) -> None:
        for encoding in ("ascii", "gbk"):
            with self.subTest(encoding=encoding):
                result = self.run_checker(encoding)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("All relative links resolve", result.stdout)
                self.assertNotIn("UnicodeEncodeError", result.stderr)

    def test_broken_link_keeps_diagnostic_with_restrictive_stdout(self) -> None:
        result = self.run_checker("ascii", broken=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("README.md: broken link -> missing-\\u2603.md", result.stdout)
        self.assertNotIn("UnicodeEncodeError", result.stderr)

    def test_utf8_retains_original_status_and_path_text(self) -> None:
        result = self.run_checker("utf-8")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("\u2713 All relative links resolve", result.stdout)
        result = self.run_checker("utf-8", broken=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("missing-\u2603.md", result.stdout)


if __name__ == "__main__":
    unittest.main()
