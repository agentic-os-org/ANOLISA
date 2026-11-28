"""Check guide lookup when a user installs a skill outside the system prefix."""

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

GUIDE = Path(__file__).resolve().parents[1] / "others" / "anolisa-guide"


class GuideInstallLocationTests(unittest.TestCase):
    def test_offline_lookup_returns_this_collections_reference_directory(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory).resolve()
            guide = shutil.copytree(GUIDE, home / "custom skills" / GUIDE.name)
            # Prevent refresh from creating a virtualenv or contacting a package index.
            venv = home / ".cache" / "anolisa" / ".venv"
            venv.parent.mkdir(parents=True)
            venv.touch()

            result = subprocess.run(
                [sys.executable, str(guide / "scripts" / "check_docs.py")],
                cwd=home,
                env={**os.environ, "HOME": str(home), "USERPROFILE": str(home)},
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(result.stdout.splitlines()[-1], str(guide / "reference"))


if __name__ == "__main__":
    unittest.main()
