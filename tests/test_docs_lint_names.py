#!/usr/bin/env python3
"""Check the naming policy against literal tracked paths, including quoted names."""

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
class NamingTests(unittest.TestCase):
    def lint_name(self, name: str, tracked: bool = True):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for language in ("en", "zh"):
                (root / "docs/user-guide" / language).mkdir(parents=True)
            (root / name).write_text("Documentation\n", encoding="utf-8")
            script = root / "lint.sh"
            script.write_bytes(SOURCE.read_bytes().replace(b"\r\n", b"\n"))
            subprocess.run(["git", "init", "-q", str(root)], check=True, capture_output=True)
            subprocess.run(["git", "config", "core.quotepath", "true"], cwd=root, check=True)
            if tracked:
                subprocess.run(["git", "add", "--", name], cwd=root, check=True, capture_output=True)
            return subprocess.run(
                [BASH, script.as_posix()], cwd=root, capture_output=True,
                text=True, encoding="utf-8", check=False
            )

    def test_unicode_names_follow_every_prohibited_suffix(self):
        for suffix in ("_CN", "_cn", "-CN", "-cn", ".zh"):
            with self.subTest(suffix=suffix):
                result = self.lint_name(f"说明{suffix}.md")
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("Illegal Chinese doc naming", result.stdout)

    @unittest.skipIf(Path("C:/").exists(), "Windows disallows newlines in filenames")
    def test_control_character_names_follow_the_policy(self):
        result = self.lint_name("line\nbreak_cn.md")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)

    def test_ascii_prohibited_name_still_fails(self):
        result = self.lint_name("guide_cn.md")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)

    def test_valid_unicode_name_is_accepted(self):
        result = self.lint_name("说明_zh.md")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_untracked_name_does_not_enter_the_gate(self):
        result = self.lint_name("说明_cn.md", tracked=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
