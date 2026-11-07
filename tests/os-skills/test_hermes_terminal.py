"""Exercise Hermes terminal gates in detached sessions and real pseudoterminals."""

import os
import re
import shlex
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

if os.name == "posix":
    import fcntl
    import pty
    import termios

SCRIPT = Path(__file__).resolve().parents[2] / "src/os-skills/ai/install-hermes/scripts/install.sh"


@unittest.skipUnless(os.name == "posix" and shutil.which("bash"), "POSIX terminal tools")
class HermesTerminalTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="hermes-terminal-")
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.home = self.base / "data"
        self.home.mkdir()
        (self.home / ".env").write_text("TELEGRAM_BOT_TOKEN=test-token\n", encoding="utf-8")
        self.marker = self.base / "commands"

    def driver(self, operation: str, extra: str = "") -> str:
        source = SCRIPT.read_text(encoding="utf-8")
        functions = []
        for name in ("can_use_tty", "prompt_yes_no", "run_setup_wizard", "maybe_start_gateway"):
            function = re.search(r"^" + name + r"\(\) \{.*?^\}", source, re.M | re.S)
            if function is not None:
                functions.append(function.group(0))
            elif name != "can_use_tty":
                self.fail(f"Missing production function {name}")
        return "\n".join(
            [
                "log_info() { printf '%s\\n' \"$*\"; }",
                "log_success() { printf '%s\\n' \"$*\"; }",
                "log_warn() { printf '%s\\n' \"$*\"; }",
                f"MARKER={shlex.quote(str(self.marker))}",
                'python() { printf \'%s\\n\' "$*" >> "$MARKER"; }',
                "IS_INTERACTIVE=false",
                "USE_VENV=false",
                "RUN_SETUP=true",
                "DISTRO=linux",
                f"INSTALL_DIR={shlex.quote(str(self.base))}",
                f"HERMES_HOME={shlex.quote(str(self.home))}",
                *functions,
                extra,
                operation,
                "printf '%s\\n' completed",
            ]
        )

    def run_detached(self, operation: str, extra: str = "") -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", "-e"],
            input=self.driver(operation, extra),
            capture_output=True,
            text=True,
            start_new_session=True,
            timeout=10,
        )

    def run_terminal(self, operation: str, answer: bytes = b"") -> subprocess.CompletedProcess[str]:
        driver = self.base / "driver.sh"
        driver.write_text(self.driver(operation), encoding="utf-8")
        master, slave = pty.openpty()
        try:
            if answer:
                os.write(master, answer)

            def acquire_terminal() -> None:
                fcntl.ioctl(0, termios.TIOCSCTTY, 0)

            return subprocess.run(
                ["bash", "-e", str(driver)],
                stdin=slave,
                capture_output=True,
                text=True,
                start_new_session=True,
                preexec_fn=acquire_terminal,
                timeout=10,
            )
        finally:
            os.close(slave)
            os.close(master)

    def test_detached_yes_prompt_uses_default_without_redirection_errors(self) -> None:
        result = self.run_detached("prompt_yes_no question yes")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertIn("completed", result.stdout)

    def test_detached_no_prompt_uses_default_without_redirection_errors(self) -> None:
        result = self.run_detached("if prompt_yes_no question no; then echo YES; else echo NO; fi")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertIn("NO\ncompleted", result.stdout)

    def test_detached_setup_is_skipped(self) -> None:
        result = self.run_detached("run_setup_wizard")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Setup wizard skipped (no terminal available)", result.stdout)
        self.assertFalse(self.marker.exists())

    def test_detached_gateway_is_skipped_before_offering_services(self) -> None:
        extra = "\n".join(
            [
                "prompt_yes_no() { printf '%s\\n' prompt >> \"$MARKER\"; return 1; }",
                "get_hermes_command_path() { printf '%s\\n' unused; }",
            ]
        )
        result = self.run_detached("maybe_start_gateway", extra)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Gateway setup skipped (no terminal available)", result.stdout)
        self.assertFalse(self.marker.exists())

    def test_unrequested_setup_still_skips(self) -> None:
        result = self.run_detached("run_setup_wizard", "RUN_SETUP=false")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("run with --with-setup", result.stdout)
        self.assertFalse(self.marker.exists())

    def test_piped_installer_can_prompt_through_controlling_terminal(self) -> None:
        result = self.run_terminal(
            "if prompt_yes_no question yes; then echo YES; else echo NO; fi", b"n\n"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertIn("NO\ncompleted", result.stdout)

    def test_setup_runs_when_controlling_terminal_is_available(self) -> None:
        result = self.run_terminal("run_setup_wizard")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.marker.read_text(encoding="utf-8"), "-m hermes_cli.main setup\n")
        self.assertIn("Starting setup wizard", result.stdout)


if __name__ == "__main__":
    unittest.main()
