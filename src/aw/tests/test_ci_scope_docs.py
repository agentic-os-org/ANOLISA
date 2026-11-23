"""Keep documented split package operations on the explicitly selected database."""

import hashlib
import os
import re
import shlex
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
GUIDES = (
    "user-guide/{language}/aw/installation.md",
    "user-guide/{language}/aw/security.md",
    "user-guide/{language}/aw/tokenless.md",
    "developer-guide/{language}/aw/reference/packages.md",
    "user-guide/{language}/aw/operations.md",
)


def mutations(text: str) -> list[list[str]]:
    """Read software commands without executing sudo, shell expansion or an installer."""
    commands = []
    for block in re.findall(r"```(?:bash|sh)\n(.*?)```", text, re.DOTALL):
        for line in block.replace("\\\n", " ").splitlines():
            words = shlex.split(line, comments=True)
            program = 1 if words[:1] == ["sudo"] else 0
            if (
                len(words) > program + 1
                and (words[program] == "aw" or words[program].endswith("/aw"))
                and words[program + 1] in ("install", "uninstall")
                and "--prefix" in words
            ):
                commands.append(words)
    return commands


def option(command: list[str], name: str) -> str:
    return command[command.index(name) + 1]


def validate_system_commands(text: str) -> list[list[str]]:
    """Root examples must select system scope rather than rely on sudo's environment."""
    commands = mutations(text)
    for command in commands:
        if command[0] == "sudo":
            if command.count("--scope") != 1 or option(command, "--scope") != "system":
                raise ValueError(f"sudo AW mutation needs explicit system scope: {command}")
    return commands


class ScopeDocumentationTests(unittest.TestCase):
    def guide(self, template: str, language: str) -> str:
        return (REPO / "docs" / template.format(language=language)).read_text(encoding="utf-8")

    def test_paired_guides_use_the_same_provider_mutations(self) -> None:
        count = 0
        for template in GUIDES:
            with self.subTest(guide=template):
                english = validate_system_commands(self.guide(template, "en"))
                chinese = validate_system_commands(self.guide(template, "zh"))
                self.assertEqual(english, chinese)
                count += len(english)
        self.assertGreater(count, 0, "guides must retain local Provider examples")

    def test_installation_uses_make_for_core_and_separate_provider_prefix(self) -> None:
        for language in ("en", "zh"):
            text = self.guide(GUIDES[0], language)
            self.assertIn('make -C "$AW_BUNDLE_DIR" install PREFIX="$AW_PREFIX"', text)
            commands = mutations(text)
            self.assertTrue(commands)
            for command in commands:
                self.assertEqual(option(command, "--prefix"), "$AW_EXTENSION_PREFIX")
                self.assertEqual(command[0], "aw")

    def test_install_example_requires_a_matching_checksum_before_extraction(self) -> None:
        for language in ("en", "zh"):
            text = self.guide(GUIDES[0], language)
            script = re.findall(r"```bash\n(.*?)```", text, re.DOTALL)[0]
            version = re.search(r"^export AW_VERSION=(.+)$", script, re.MULTILINE).group(1)
            archive = f"aw-core-{version}-linux-{os.uname().machine}.tar.gz"
            payload = b"selected archive"
            digest = hashlib.sha256(payload).hexdigest()
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / archive).write_bytes(payload)
                (root / "sdk-candidate.json").write_bytes(b"{}")
                receipt = hashlib.sha256(b"{}").hexdigest() + "  sdk-candidate.json\n"
                for command in ("tar", "make", "aw"):
                    stub = root / command
                    stub.write_text(f"#!/bin/sh\necho {command} >> calls\n")
                    stub.chmod(0o755)
                for entry, valid in (("", False), ("0" * 64 + "  " + archive, False),
                                     (digest + "  " + archive, True)):
                    with self.subTest(language=language, entry=entry):
                        (root / "SHA256SUMS").write_text(receipt + entry + "\n")
                        calls = root / "calls"
                        calls.write_text("")
                        result = subprocess.run(
                            ["bash", "-c", script], cwd=root, timeout=5, capture_output=True,
                            env={**os.environ, "HOME": directory, "PATH": f"{root}:/usr/bin:/bin"},
                        )
                        self.assertEqual(result.returncode == 0, valid, result.stderr)
                        self.assertEqual(calls.read_text().splitlines(), ["tar", "make", "aw"] if valid else [])

    def test_developer_package_example_uses_one_scope_and_prefix(self) -> None:
        for language in ("en", "zh"):
            commands = validate_system_commands(self.guide(GUIDES[3], language))
            with self.subTest(language=language):
                self.assertEqual([command[1] for command in commands],
                                 ["install", "install", "uninstall"])
                self.assertEqual([option(command, "--scope") for command in commands],
                                 ["user"] * 3)
                self.assertEqual([option(command, "--prefix") for command in commands],
                                 ["$AW_PREFIX"] * 3)

    def test_documented_installs_never_use_a_bundle_bootstrap_command(self) -> None:
        for template in GUIDES:
            for language in ("en", "zh"):
                for command in mutations(self.guide(template, language)):
                    with self.subTest(guide=template, language=language, command=command):
                        program = command[1] if command[0] == "sudo" else command[0]
                        self.assertEqual(program, "aw")

    def test_missing_wrong_and_duplicate_scope_in_root_examples_are_rejected(self) -> None:
        template = (
            '```bash\nsudo "./bundle/aw" install {scope} --bundle /bundle --prefix /prefix\n```'
        )
        for scope in ("", "--scope user", "--scope system --scope system"):
            with self.subTest(scope=scope), self.assertRaisesRegex(ValueError, "explicit system"):
                validate_system_commands(template.format(scope=scope))
        accepted = validate_system_commands(template.format(scope="--scope system"))
        self.assertEqual(option(accepted[0], "--scope"), "system")


if __name__ == "__main__":
    unittest.main()
