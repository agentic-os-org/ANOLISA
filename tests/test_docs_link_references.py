"""Reference destinations through the complete checker CLI and real Git."""

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SOURCE_ROOT = Path(
    os.environ.get("ANOLISA_SOURCE_ROOT", Path(__file__).resolve().parents[1])
)
SCRIPT = SOURCE_ROOT / "scripts/docs-link-check.py"


class ReferenceLinkCliTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.environment = dict(os.environ, PYTHONIOENCODING="utf-8")
        self.git("init", "--quiet")
        (self.root / "guide.md").write_text("# Guide\n", encoding="utf-8")
        (self.root / "guide with spaces.md").write_text("# Guide\n", encoding="utf-8")

    def git(self, *arguments: str) -> None:
        subprocess.run(
            ["git", *arguments],
            cwd=self.root,
            env=self.environment,
            capture_output=True,
            check=True,
            timeout=30,
        )

    def check(self, markdown: str) -> subprocess.CompletedProcess:
        (self.root / "README.md").write_text(markdown + "\n", encoding="utf-8")
        # The exact PR9236 head enumerates tracked files. Main's current-main
        # composition also sees these same files without relying on that change.
        self.git("add", ".")
        return subprocess.run(
            [sys.executable, str(SCRIPT)],
            cwd=self.root,
            env=self.environment,
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=False,
            timeout=30,
        )

    def assert_missing(self, result: subprocess.CompletedProcess, target: str) -> None:
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("1 broken relative link(s)", result.stdout)
        self.assertIn("README.md: broken link -> " + target, result.stdout)
        self.assertNotIn("Traceback", result.stderr)

    def assert_clean(self, result: subprocess.CompletedProcess) -> None:
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("All relative links resolve", result.stdout)

    def test_full_reference_missing_target(self) -> None:
        self.assert_missing(
            self.check("[Install][guide]\n\n[guide]: missing-guide.md"),
            "missing-guide.md",
        )

    def test_collapsed_reference_missing_target(self) -> None:
        self.assert_missing(
            self.check("[guide][]\n\n[guide]: missing-guide.md"), "missing-guide.md"
        )

    def test_shortcut_reference_missing_target(self) -> None:
        self.assert_missing(
            self.check("[guide]\n\n[guide]: missing-guide.md"), "missing-guide.md"
        )

    def test_reference_image_missing_target(self) -> None:
        self.assert_missing(
            self.check("![diagram][image]\n\n[image]: missing.png"), "missing.png"
        )

    def test_normalized_label_and_first_definition(self) -> None:
        self.assert_missing(
            self.check(
                "[Install][  USER   GUIDE ]\n\n"
                "[user guide]: missing-guide.md\n[user guide]: guide.md"
            ),
            "missing-guide.md",
        )
        self.assert_clean(
            self.check(
                "[Install][guide]\n\n[guide]: guide.md\n[GUIDE]: missing-guide.md"
            )
        )

    def test_unused_and_undefined_reference_controls(self) -> None:
        self.assert_clean(self.check("[not defined]\n\n[unused]: missing-guide.md\n"))

    def test_existing_external_fragment_and_encoded_destinations(self) -> None:
        self.assert_clean(
            self.check(
                "[guide][local] ![image][encoded] [web] [section]\n\n"
                "[local]: guide.md\n"
                "[encoded]: <guide with spaces.md> 'display title'\n"
                "[web]: HTTPS://example.com/guide\n"
                "[section]: #intro"
            )
        )
        self.assert_missing(
            self.check(
                '[guide][missing]\n\n[missing]: missing%20guide.md?view=1#intro "display title"'
            ),
            "missing%20guide.md?view=1#intro",
        )

    def test_reference_code_examples_are_not_uses(self) -> None:
        self.assert_clean(
            self.check(
                "~~~markdown\n[guide]\n~~~\n\n"
                "    [guide]\n\n"
                "`[guide]` and ``[guide]``\n\n"
                "[guide]: missing-guide.md"
            )
        )
        self.assert_clean(self.check("~~~markdown\n[guide]\n[guide]: missing-guide.md"))
        self.assert_clean(
            self.check(
                "`example\n[guide]: missing-guide.md\nexample`\n\n"
                "[guide]: guide.md\n\n[guide]"
            )
        )
        self.assert_clean(
            self.check(
                "> ~~~markdown\n> [guide]\n> ~~~\n\n"
                "- ~~~markdown\n  [guide]\n  ~~~\n\n"
                "[guide]: missing-guide.md"
            )
        )

    def test_escaped_openers_and_inline_link_labels_are_not_uses(self) -> None:
        self.assert_clean(
            self.check(
                r"\[guide] [guide](guide.md) [[guide]](guide.md)" + "\n\n"
                "[guide]: missing-guide.md"
            )
        )
        self.assert_clean(
            self.check(
                '[guide](guide.md "see [other][guide]")\n\n[guide]: missing-guide.md'
            )
        )

    def test_unsupported_first_definition_does_not_adopt_later_duplicate(self) -> None:
        self.assert_clean(
            self.check("[guide]\n\n[guide]:\n  <guide.md>\n\n[guide]: missing-guide.md")
        )
        self.assert_clean(
            self.check(
                '[guide]\n\n[guide]: guide.md\n  "title [other]"\n\n'
                "[guide]: missing-guide.md\n[other]: missing-other.md"
            )
        )
        self.assert_clean(
            self.check("> [guide]: guide.md\n\n[guide]: missing-guide.md\n\n[guide]")
        )

    def test_reference_in_html_comment_is_not_a_use(self) -> None:
        self.assert_clean(self.check("<!-- [guide] -->\n\n[guide]: missing-guide.md"))
        self.assert_clean(
            self.check(
                "<!--\n[guide]: missing-guide.md\n-->\n\n"
                "[guide]: guide.md\n\n[guide]"
            )
        )
        self.assert_clean(self.check("[guide]: missing-guide.md\n\n<!-- [guide]"))
        self.assert_missing(
            self.check("Text <!-- hidden --> [guide]\n\n[guide]: missing-guide.md"),
            "missing-guide.md",
        )
        self.assert_missing(
            self.check("`<!--` [guide]\n\n[guide]: missing-guide.md"),
            "missing-guide.md",
        )

    def test_inline_missing_destination_still_fails(self) -> None:
        self.assert_missing(
            self.check("[Install](missing-guide.md)"), "missing-guide.md"
        )


if __name__ == "__main__":
    unittest.main()
