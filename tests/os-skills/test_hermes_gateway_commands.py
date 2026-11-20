"""Verify real command argv through installer gateway paths without services."""

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


@unittest.skipUnless(os.name == "posix" and shutil.which("bash"), "POSIX shell commands")
class HermesGatewayCommandTests(unittest.TestCase):
    def run_gateway(
        self,
        *,
        name: str,
        termux: bool = False,
        whatsapp: bool = False,
        fail_install: bool = False,
        declined: bool = False
    ) -> list[list[str]]:
        with tempfile.TemporaryDirectory(prefix="hermes-gateway-") as temporary:
            root = Path(temporary)
            command = root / name / "hermes"
            command.parent.mkdir()
            record = root / "commands.jsonl"
            recorder = root / "record.py"
            recorder.write_text(
                "import json, os, sys\n"
                "with open(os.environ['RECORD'], 'a') as f: "
                "f.write(json.dumps(sys.argv[1:]) + '\\n')\n"
                "sys.exit(7 if os.environ['FAIL_INSTALL'] == '1' "
                "and sys.argv[1:] == ['gateway', 'install'] else 0)\n",
                encoding="utf-8",
            )
            command.write_text(
                '#!/bin/sh\nexec "$TEST_PYTHON" "$RECORDER" "$@"\n', encoding="utf-8"
            )
            command.chmod(0o755)
            home = root / "data"
            (home / "logs").mkdir(parents=True)
            (home / ".env").write_text(
                "WHATSAPP_ENABLED=true\n" if whatsapp else "TELEGRAM_BOT_TOKEN=fixture\n",
                encoding="utf-8",
            )
            bin_dir = root / "bin"
            bin_dir.mkdir()
            systemctl = bin_dir / "systemctl"
            systemctl.write_text("#!/bin/sh\nexit 99\n", encoding="utf-8")
            systemctl.chmod(0o755)
            source = SCRIPT.read_text(encoding="utf-8")
            function = re.search(r"^maybe_start_gateway\(\) \{.*?^\}", source, re.M | re.S).group()
            shell = root / "run.sh"
            shell.write_text(
                "set -e\nlog_info(){ :; }; log_warn(){ :; }; log_success(){ :; }\n"
                'get_hermes_command_path(){ printf "%s\\n" "$HERMES_TEST_COMMAND"; }\n'
                'prompt_yes_no(){ [ "$DECLINED" = 0 ]; }\n'
                "can_use_tty(){ return 0; }\n" + function + "\nmaybe_start_gateway\nwait\n",
                encoding="utf-8",
            )
            environment = os.environ.copy()
            environment.update(
                HERMES_HOME=str(home),
                HERMES_TEST_COMMAND=str(command),
                IS_INTERACTIVE="true",
                DISTRO="termux" if termux else "linux",
                TEST_PYTHON=sys.executable,
                RECORDER=str(recorder),
                RECORD=str(record),
                FAIL_INSTALL="1" if fail_install else "0",
                DECLINED="1" if declined else "0",
                PATH=str(bin_dir) + os.pathsep + environment["PATH"],
            )
            result = subprocess.run(
                ["bash", str(shell)], env=environment, capture_output=True, text=True, timeout=10
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            return (
                [json.loads(line) for line in record.read_text().splitlines()]
                if record.exists()
                else []
            )

    def test_service_install_and_start_keep_a_space_containing_command_path(self) -> None:
        self.assertEqual(
            self.run_gateway(name="Hermes Agent"), [["gateway", "install"], ["gateway", "start"]]
        )

    def test_whatsapp_pairing_keeps_the_command_path(self) -> None:
        self.assertEqual(
            self.run_gateway(name="Hermes Agent", whatsapp=True),
            [["whatsapp"], ["gateway", "install"], ["gateway", "start"]],
        )

    def test_background_launch_keeps_the_command_path(self) -> None:
        self.assertEqual(self.run_gateway(name="Hermes Agent", termux=True), [["gateway"]])

    def test_literal_glob_characters_in_command_path_are_preserved(self) -> None:
        self.assertEqual(
            self.run_gateway(name="Hermes [dev] *"), [["gateway", "install"], ["gateway", "start"]]
        )

    def test_failed_service_install_does_not_start_the_gateway(self) -> None:
        self.assertEqual(
            self.run_gateway(name="hermes-bin", fail_install=True), [["gateway", "install"]]
        )

    def test_declined_setup_invokes_no_command(self) -> None:
        self.assertEqual(self.run_gateway(name="Hermes Agent", declined=True), [])

    def test_ordinary_path_keeps_existing_argument_order(self) -> None:
        self.assertEqual(
            self.run_gateway(name="hermes-bin"), [["gateway", "install"], ["gateway", "start"]]
        )


if __name__ == "__main__":
    unittest.main()
