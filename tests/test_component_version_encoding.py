"""The synchronized repository passes without Python UTF-8 locale mode."""

import os
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


class VersionEncodingTests(unittest.TestCase):
    def test_repository_metadata_has_a_fixed_utf8_encoding(self) -> None:
        environment = dict(os.environ, PYTHONUTF8="0", PYTHONCOERCECLOCALE="0", LC_ALL="C")
        result = subprocess.run(
            [sys.executable, str(ROOT / "scripts/check-component-versions.py")],
            cwd=ROOT,
            env=environment,
            capture_output=True,
            text=True,
            encoding="utf-8",
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("metadata is synchronized", result.stdout)


if __name__ == "__main__":
    unittest.main()
