#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for the skill-regression discoverer itself.

`run-skill-regressions.sh` is the CI entry for everything the skills keep beside
their scripts, so the entry needs a check of its own — twice over, since an
entry that quietly matches nothing prints success (review of #7785, rounds 2
and 3).

Two kinds of case:

- a throwaway tree (the runner copied in, a passing shell script, a failing
  shell script, a passing and a failing Python script, and a `test-module.sh`
  -style system script) pins the mechanism: matched scripts run and are named,
  a failure in either language fails the entry, and the system script is skipped
  while the same tree's real regression still runs;
- `--list` against this repository's **real** tree pins the coverage: the
  discovery rule must still match the files that exist, and the installer's
  regression is looked up from the directory it lives in rather than from a
  literal here — otherwise a rename would turn this case into a tautology.

Usage: python3 src/os-skills/tests/test_skill_regression_runner.py
"""

import subprocess
import tempfile
import unittest
from pathlib import Path

TESTS_DIR = Path(__file__).resolve().parent
REPO_RUNNER = TESTS_DIR / "run-skill-regressions.sh"
SKILLS_ROOT = TESTS_DIR.parent


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
        (scripts / "test_passing.py").write_text(
            'print("PASSING-PYTHON-RAN")\n', encoding="utf-8"
        )
        (scripts / "test_failing.sh").write_text(
            "#!/usr/bin/env bash\necho FAILING-REGRESSION-RAN\nexit %d\n" % (1 if failing else 0),
            encoding="utf-8",
        )
        (scripts / "test_failing.py").write_text(
            'import sys\nprint("FAILING-PYTHON-RAN")\nsys.exit(%d)\n' % (1 if failing else 0),
            encoding="utf-8",
        )
        # The system-script shape: a real machine, not a sandbox.
        (scripts / "test-module.sh").write_text(
            "#!/usr/bin/env bash\necho SYSTEM-SCRIPT-RAN\nexit 0\n", encoding="utf-8"
        )
        tests = root / "tests"
        tests.mkdir()
        (tests / "run-skill-regressions.sh").write_text(
            REPO_RUNNER.read_text(encoding="utf-8"), encoding="utf-8"
        )
        return root

    def run_runner(self, root, *args):
        return subprocess.run(
            ["bash", str(root / "tests" / "run-skill-regressions.sh"), *args],
            capture_output=True,
            text=True,
            timeout=120,
        )

    # --- the mechanism, on a throwaway tree ------------------------------

    def test_matched_scripts_run_and_are_named(self):
        result = self.run_runner(self.build_tree())
        self.assertIn("PASSING-REGRESSION-RAN", result.stdout)
        self.assertIn("PASSING-PYTHON-RAN", result.stdout, "the .py arm must run too")
        self.assertIn("demo-skill/scripts/test_passing.sh", result.stdout)
        self.assertIn("demo-skill/scripts/test_passing.py", result.stdout)
        self.assertIn("ran 4 skill regression(s)", result.stdout)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_a_failure_in_either_language_fails_the_entry(self):
        result = self.run_runner(self.build_tree(failing=True))
        self.assertIn("FAILING-REGRESSION-RAN", result.stdout)
        self.assertIn("FAILING-PYTHON-RAN", result.stdout)
        self.assertNotEqual(result.returncode, 0, "a failing regression must fail the CI entry")
        self.assertIn("FAILED:", result.stdout + result.stderr)

    def test_system_scripts_are_skipped_while_the_rest_still_runs(self):
        result = self.run_runner(self.build_tree())
        self.assertNotIn("SYSTEM-SCRIPT-RAN", result.stdout,
                         "test-module.sh needs a real machine and must stay out of the discoverer")
        self.assertIn("test-module.sh (needs a real machine, skipped)", result.stdout)
        # Positive control: without it this case would also pass on a runner that
        # discovers nothing at all.
        self.assertIn("PASSING-REGRESSION-RAN", result.stdout)

    def test_a_tree_without_regressions_is_a_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tests = root / "tests"
            tests.mkdir()
            (tests / "run-skill-regressions.sh").write_text(
                REPO_RUNNER.read_text(encoding="utf-8"), encoding="utf-8"
            )
            result = self.run_runner(root)

        self.assertNotEqual(result.returncode, 0, "finding nothing must not read as success")
        self.assertIn("no skill regressions found", result.stdout + result.stderr)

    # --- the coverage, against this repository ---------------------------

    def test_the_real_tree_is_discovered_and_names_the_install_regression(self):
        result = subprocess.run(
            ["bash", str(REPO_RUNNER), "--list"],
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        listed = [
            line for line in result.stdout.splitlines()
            if line and not line.startswith(("===", "found "))
        ]
        self.assertGreater(
            len(listed), 0, "the discovery rule matched nothing in this repository"
        )
        # Look the regression up beside the installer rather than hardcoding its
        # name: a rename must make this case fail, not become a tautology.
        beside_setup = sorted(
            path.relative_to(SKILLS_ROOT).as_posix()
            for path in (SKILLS_ROOT / "ai" / "install-qwenpaw" / "scripts").glob("test*")
            if path.is_file()
        )
        self.assertTrue(beside_setup, "no regression sits beside the qwenpaw installer")
        for path in beside_setup:
            self.assertIn(path, listed, "the installer's regression must be discovered")
        self.assertNotIn(
            "devops/kernel-dev/scripts/test-module.sh", listed,
            "the real-machine script stays excluded on the real tree too",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
