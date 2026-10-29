"""Keep documented split package operations on the explicitly selected database."""

from pathlib import Path
import re
import shlex
import unittest


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

    def test_paired_guides_use_identical_explicit_system_mutations(self) -> None:
        for template in GUIDES:
            with self.subTest(guide=template):
                english = validate_system_commands(self.guide(template, "en"))
                chinese = validate_system_commands(self.guide(template, "zh"))
                self.assertTrue(english, "guide must retain its runnable installation example")
                self.assertEqual(english, chinese)

    def test_user_guide_core_and_extensions_select_the_same_scope_and_prefix(self) -> None:
        for language in ("en", "zh"):
            commands = [
                validate_system_commands(self.guide(template, language))[0]
                for template in GUIDES[:3]
            ]
            removals = [
                command
                for command in validate_system_commands(self.guide(GUIDES[4], language))
                if command[0] == "sudo"
            ]
            self.assertEqual(len(removals), 1)
            commands.extend(removals)
            with self.subTest(language=language):
                self.assertEqual([option(command, "--scope") for command in commands],
                                 ["system"] * 4)
                self.assertEqual([option(command, "--prefix") for command in commands],
                                 ["$AW_PREFIX"] * 4)
                self.assertIn("--bundle", commands[0])
                self.assertIn("$AW_SEC_BUNDLE/aw", commands[1][1])
                self.assertIn("$AW_TOKENLESS_BUNDLE/aw", commands[2][1])
                self.assertEqual(commands[3][2], "uninstall")

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

    def test_missing_wrong_and_duplicate_scope_in_root_examples_are_rejected(self) -> None:
        template = '```bash\nsudo "./bundle/aw" install {scope} --bundle /bundle --prefix /prefix\n```'
        for scope in ("", "--scope user", "--scope system --scope system"):
            with self.subTest(scope=scope), self.assertRaisesRegex(ValueError, "explicit system"):
                validate_system_commands(template.format(scope=scope))
        accepted = validate_system_commands(template.format(scope="--scope system"))
        self.assertEqual(option(accepted[0], "--scope"), "system")


if __name__ == "__main__":
    unittest.main()
