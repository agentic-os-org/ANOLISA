"""Exercise real installer option parsing without running installation."""

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "src/os-skills/ai/install-hermes/scripts/install.sh"


@unittest.skipUnless(os.name == "posix" and shutil.which("bash"), "POSIX shell options")
class HermesOptionValuesTests(unittest.TestCase):
    def parse(self, arguments: list[str]) -> subprocess.CompletedProcess:
        with tempfile.TemporaryDirectory(prefix="hermes-options-") as temporary:
            root = Path(temporary)
            source = SCRIPT.read_text(encoding="utf-8")
            self.assertRegex(source, r"\nmain\s*$")
            source = re.sub(r"\nmain\s*$", "\n", source)
            source += (
                '"$TEST_PYTHON" -c "import json,sys; print(json.dumps(sys.argv[1:]))" '
                'PARSED "$BRANCH" "$INSTALL_DIR" "$HERMES_HOME" "$SKIP_TOKENLESS"\n'
            )
            script = root / "parse.sh"
            script.write_text(source, encoding="utf-8")
            environment = os.environ.copy()
            environment["TEST_PYTHON"] = sys.executable
            environment.pop("HERMES_HOME", None)
            environment.pop("HERMES_INSTALL_DIR", None)
            return subprocess.run(
                ["bash", str(script), *arguments],
                env=environment,
                capture_output=True,
                text=True,
                timeout=10,
            )

    def assert_invalid(self, arguments: list[str], option: str) -> None:
        result = self.parse(arguments)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(option, result.stderr)
        self.assertIn("value", result.stderr.lower())
        self.assertNotIn("PARSED", result.stdout)

    def test_value_options_require_a_following_argument(self) -> None:
        for option in ("--branch", "--dir", "--hermes-home"):
            with self.subTest(option=option):
                self.assert_invalid([option], option)

    def test_value_options_do_not_consume_the_next_option(self) -> None:
        for option in ("--branch", "--dir", "--hermes-home"):
            for next_option in ("--skip-tokenless", "-h"):
                with self.subTest(option=option, next_option=next_option):
                    self.assert_invalid([option, next_option], option)

    def test_empty_values_are_rejected(self) -> None:
        for option in ("--branch", "--dir", "--hermes-home"):
            with self.subTest(option=option):
                self.assert_invalid([option, ""], option)

    def test_valid_paths_with_spaces_and_branch_values_are_preserved(self) -> None:
        result = self.parse(
            [
                "--branch",
                "feature/check",
                "--dir",
                "/tmp/Hermes checkout",
                "--hermes-home",
                "/tmp/Hermes data",
                "--skip-tokenless",
            ]
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(result.stdout),
            ["PARSED", "feature/check", "/tmp/Hermes checkout", "/tmp/Hermes data", "true"],
        )

    def test_repeated_values_use_the_last_option(self) -> None:
        result = self.parse(["--branch", "first", "--branch", "second"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)[1], "second")

    def test_explicit_relative_dash_name_is_supported(self) -> None:
        result = self.parse(["--dir", "./--checkout"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)[2], "./--checkout")

    def test_help_stops_before_installer_actions(self) -> None:
        result = self.parse(["--help"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Usage:", result.stdout)
        self.assertNotIn("PARSED", result.stdout)


if __name__ == "__main__":
    unittest.main()
