"""Guard that no test module ships inside a skill's script directory.

Tests belong in `src/os-skills/tests/`, the only directory the prebuilt CI
job discovers. A `test_*.py` left under `others/*/scripts/` is silently
never run *and* is copied into the package, because the packaging spec
copies each skill directory wholesale.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[1]
OTHERS = ROOT / "others"


class SkillScriptTestsTests(unittest.TestCase):
    def test_no_test_modules_live_in_skill_scripts(self) -> None:
        misplaced = sorted(
            str(path.relative_to(ROOT))
            for path in OTHERS.glob("*/scripts/*.py")
            if path.name.startswith("test_") or path.name.endswith("_test.py")
        )
        self.assertEqual(
            misplaced,
            [],
            "these test modules live under a skill's scripts/ directory, so CI "
            "never runs them and the package ships them; move them to "
            "src/os-skills/tests/",
        )

    def test_every_module_in_tests_is_discovered(self) -> None:
        # `unittest discover` defaults to the `test*.py` pattern, so a module
        # named `*_test.py` sits in the discovery root and still never runs.
        undiscovered = sorted(
            path.name
            # Only names that are plainly meant to be tests: a shared helper
            # module in this directory is legitimate and must not be flagged.
            for path in (ROOT / "tests").glob("*_test.py")
        )
        self.assertEqual(
            undiscovered,
            [],
            "these modules are named like tests but do not match the "
            "`test*.py` discovery pattern, so CI never runs them",
        )


if __name__ == "__main__":
    unittest.main()
