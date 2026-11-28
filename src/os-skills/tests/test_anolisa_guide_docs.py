"""Check that the bundled ANOLISA guide resolves its pages beside the script."""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SKILL = ROOT / "others" / "anolisa-guide"


class AnolisaGuideDocsTests(unittest.TestCase):
    def test_custom_install_uses_bundled_pages(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            skill = Path(temporary) / "custom-collection" / "anolisa-guide"
            (skill / "scripts").mkdir(parents=True)
            (skill / "reference").mkdir()
            shutil.copyfile(
                SKILL / "scripts" / "check_docs.py",
                skill / "scripts" / "check_docs.py",
            )
            stamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
            for index in range(13):
                (skill / "reference" / f"page-{index}.md").write_text(
                    f"> **爬取时间**: {stamp}\n", encoding="utf-8"
                )

            result = subprocess.run(
                [sys.executable, str(skill / "scripts" / "check_docs.py")],
                capture_output=True,
                text=True,
                encoding="utf-8",
                env=dict(os.environ, PYTHONIOENCODING="utf-8"),
                timeout=30,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(str(skill / "reference"), result.stdout)


if __name__ == "__main__":
    unittest.main()
