"""PR9236 review5697349074 through the complete CLI and genuine Git/FS.

New review-only cases; the twelve accepted reference cases and title dependency
remain frozen. No Markdown renderer, checker function, or Git output is mocked.
"""

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


class ReferenceReviewCliTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.environment = dict(os.environ, PYTHONIOENCODING="utf-8")
        self.git("init", "--quiet")
        (self.root / "guide.md").write_text("# Guide\n", encoding="utf-8")
        (self.root / "diagram.png").write_bytes(
            b"existing target; image decoding is outside this CLI"
        )

    def git(self, *arguments):
        subprocess.run(
            ["git", *arguments],
            cwd=self.root,
            env=self.environment,
            capture_output=True,
            check=True,
            timeout=30,
        )

    def check(self, markdown):
        (self.root / "README.md").write_text(markdown + "\n", encoding="utf-8")
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

    def assert_missing(self, markdown, target):
        result = self.check(markdown)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("1 broken relative link(s)", result.stdout)
        self.assertIn("README.md: broken link -> " + target, result.stdout)
        self.assertNotIn("Traceback", result.stderr)

    def assert_clean(self, markdown):
        result = self.check(markdown)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("All relative links resolve", result.stdout)

    def test_code_bearing_shortcut_does_not_adopt_mask_label(self):
        self.assert_clean("[read `API`]\n\n[read `API`]: guide.md\n[read]: missing.md")

    def test_code_bearing_full_reference_checks_original_label(self):
        self.assert_missing(
            "[read][read `API`]\n\n[read `API`]: missing.md", "missing.md"
        )

    def test_code_bearing_collapsed_reference_checks_original_label(self):
        self.assert_missing("[read `API`][]\n\n[read `API`]: missing.md", "missing.md")

    def test_comment_bearing_label_does_not_adopt_mask_label(self):
        self.assert_clean(
            "[read API]\n\n"
            "[read <!-- note --> API]: missing.md\n[read API]: guide.md"
        )

    def test_invalid_definition_preserves_real_inline_link(self):
        self.assert_missing(
            "[unused]: not a definition [Install](missing.md)", "missing.md"
        )

    def test_incomplete_title_candidate_preserves_real_inline_link(self):
        self.assert_missing(
            "[unused]: guide.md\n(incomplete title [Install](missing.md)", "missing.md"
        )

    def test_trailing_comment_does_not_promote_invalid_definition(self):
        self.assert_missing(
            "[guide]: guide.md <!-- not a definition -->\n\n" "[guide]: missing.md",
            "missing.md",
        )

    def test_valid_continued_title_is_not_prose_control(self):
        self.assert_clean(
            '[guide]: guide.md\n  "title [example](missing.md)\n  continued title"\n\n[guide]'
        )

    def test_invalid_definition_does_not_create_paragraph_boundary_control(self):
        self.assert_clean("[unused]: not a definition\n[guide]: missing.md\n\n[guide]")

    def test_reference_image_inside_inline_outer_link(self):
        self.assert_missing(
            "[![diagram][pic]](guide.md)\n\n[pic]: missing.png", "missing.png"
        )

    def test_reference_image_inside_reference_outer_link(self):
        self.assert_missing(
            "[![diagram][pic]][page]\n\n[pic]: missing.png\n[page]: guide.md",
            "missing.png",
        )

    def test_inner_shortcut_survives_unresolved_outer_label(self):
        self.assert_missing("[outer [guide]]\n\n[guide]: missing.md", "missing.md")

    def test_nested_existing_image_and_title_examples_control(self):
        self.assert_clean(
            '[![diagram][pic]](guide.md "see [example][missing]")\n\n'
            "[pic]: diagram.png\n[missing]: missing.md"
        )

    def test_unmatched_inline_candidate_does_not_consume_document(self):
        self.assert_missing("[plain](\n\n[guide]\n\n[guide]: missing.md", "missing.md")

    def test_balanced_invalid_inline_candidate_falls_back_to_shortcut(self):
        self.assert_missing("[guide](not a link)\n\n[guide]: missing.md", "missing.md")

    def test_valid_inline_link_still_wins_over_shortcut_control(self):
        self.assert_clean('[guide](guide.md "[other][guide]")\n\n[guide]: missing.md')

    def test_definition_immediately_after_atx_heading(self):
        self.assert_missing("# Guide\n[guide]: missing.md\n\n[guide]", "missing.md")

    def test_definition_immediately_after_thematic_break(self):
        self.assert_missing("***\n[guide]: missing.md\n\n[guide]", "missing.md")

    def test_definition_immediately_after_setext_heading(self):
        self.assert_missing(
            "Guide\n=====\n[guide]: missing.md\n\n[guide]", "missing.md"
        )

    def test_standalone_equals_prose_is_not_a_heading_control(self):
        self.assert_clean("=====\n[guide]: missing.md\n\n[guide]")

    def test_definition_still_cannot_interrupt_ordinary_paragraph_control(self):
        self.assert_clean("Ordinary paragraph\n[guide]: missing.md\n\n[guide]")


if __name__ == "__main__":
    unittest.main()
