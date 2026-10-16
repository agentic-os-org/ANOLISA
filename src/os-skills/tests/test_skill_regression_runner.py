#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for the skill-regression discoverer itself.

`run-skill-regressions.sh` is the CI entry for everything the skills keep beside
their scripts, so the entry needs a check of its own: a discoverer that quietly
matches nothing would let a broken installer ship with a green package job
(review of #7785, [P2]).

The cases build a throwaway tree with one passing script, one failing script and
one `test-module.sh`-style system script, put the runner in place of that tree's
`tests/` directory, and drive it:

- a script the rule matches runs, and its output is in the runner's;
- a failure in a matched script makes the runner exit non-zero;
- `test-module.sh` (the naming this discoverer deliberately leaves to a real
  machine) is not executed.

Usage: python3 src/os-skills/tests/test_skill_regression_runner.py
"""

import subprocess
import tempfile
import unittest
from pathlib import Path

RUNNER = Path(__file__).with_name("run-skill-regressions.sh")


class SkillRegressionRunnerTests(unittest.TestCase):
    def build_tree(self, failing=False):
        """A minimal os-skills-shaped tree with the runner installed in it."""
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        root = Path(tmp.name)
        scripts = root / "demo-skill" / "scripts"
        scripts.mkdir(parents=True)
        (scripts / "test_passing.sh").write_text(
            "#!/usr/bin/env bash\necho PASSING-REGRESSION-RAN\nexit 0\n", encoding="utf-8"
        )
        (scripts / "test_failing.sh").write_text(
            "#!/usr/bin/env bash\necho FAILING-REGRESSION-RAN\nexit %d\n" % (1 if failing else 0),
            encoding="utf-8",
        )
        # The system-script shape: a real machine, not a sandbox.
        (scripts / "test-module.sh").write_text(
            "#!/usr/bin/env bash\necho SYSTEM-SCRIPT-RAN\nexit 0\n", encoding="utf-8"
        )
        tests = root / "tests"
        tests.mkdir()
        (tests / "run-skill-regressions.sh").write_text(RUNNER.read_text(encoding="utf-8"), encoding="utf-8")
        return root

    def run_runner(self, root):
        return subprocess.run(
            ["bash", str(root / "tests" / "run-skill-regressions.sh")],
            capture_output=True,
            text=True,
            timeout=120,
        )

    def test_matched_scripts_run_and_are_named(self):
        result = self.run_runner(self.build_tree())
        self.assertIn("PASSING-REGRESSION-RAN", result.stdout)
        self.assertIn("demo-skill/scripts/test_passing.sh", result.stdout)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_a_failure_in_a_matched_script_fails_the_entry(self):
        result = self.run_runner(self.build_tree(failing=True))
        self.assertIn("FAILING-REGRESSION-RAN", result.stdout)
        self.assertNotEqual(result.returncode, 0, "a failing regression must fail the CI entry")
        self.assertIn("FAILED:", result.stdout + result.stderr)

    def test_system_scripts_are_left_alone(self):
        result = self.run_runner(self.build_tree())
        self.assertNotIn("SYSTEM-SCRIPT-RAN", result.stdout,
                         "test-module.sh needs a real machine and must stay out of the discoverer")


if __name__ == "__main__":
    unittest.main(verbosity=2)
