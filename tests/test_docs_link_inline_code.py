#!/usr/bin/env python3
"""Inline Markdown examples must not be interpreted as active links."""

import importlib.util
import tempfile
import unittest
from pathlib import Path

SOURCE = Path(__file__).resolve().parents[1] / "scripts/docs-link-check.py"
SPEC = importlib.util.spec_from_file_location("inline_link_checker", SOURCE)
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)


class InlineCodeTests(unittest.TestCase):
    def errors(self, text):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            document = root / "guide.md"
            document.write_text(text, encoding="utf-8")
            (root / "existing`file`.md").touch()
            return checker.check_file(document, root)

    def test_literal_link_and_image_examples_are_skipped(self):
        self.assertEqual(
            self.errors("Use `[link](missing.md)` and `![image](missing.png)`."), []
        )

    def test_matching_run_length_controls_the_closer(self):
        self.assertEqual(self.errors("Use ``a ` [link](missing.md) ` b``."), [])

    def test_soft_line_breaks_remain_inside_the_span(self):
        self.assertEqual(self.errors("Use `a\n[link](missing.md)\nb`."), [])

    def test_unmatched_runs_do_not_hide_real_links(self):
        self.assertEqual(
            len(self.errors("An unmatched `` opener [link](missing.md) ` end.")), 1
        )

    def test_escaped_opener_does_not_start_code(self):
        self.assertEqual(len(self.errors(r"\`[link](missing.md)`")), 1)

    def test_even_backslashes_allow_an_opener(self):
        self.assertEqual(self.errors(r"\\`[link](missing.md)`"), [])

    def test_backslash_does_not_escape_a_closer_inside_code(self):
        errors = self.errors(r"`[example](missing.md)\` [real](also-missing.md)")
        self.assertEqual(len(errors), 1)
        self.assertIn("also-missing.md", errors[0])

    def test_real_links_keep_code_in_the_label_or_destination(self):
        self.assertEqual(self.errors("[a `label`](existing`file`.md)"), [])
        self.assertEqual(len(self.errors("[a `label`](missing.md)")), 1)

    def test_spans_do_not_cross_blank_paragraph_boundaries(self):
        self.assertEqual(len(self.errors("Unmatched `\n\n[real](missing.md) `")), 1)

    def test_real_link_after_a_span_is_checked(self):
        errors = self.errors("`[example](placeholder.md)` [real](missing.md)")
        self.assertEqual(len(errors), 1)
        self.assertIn("missing.md", errors[0])

    def test_spans_do_not_cross_heading_or_list_boundaries(self):
        for text in (
            "Unmatched `\n# Heading\n[real](missing.md) `",
            "- Unmatched `\n- [real](missing.md) `",
        ):
            with self.subTest(text=text):
                self.assertEqual(len(self.errors(text)), 1)

    def test_quoted_soft_lines_share_one_span(self):
        self.assertEqual(
            self.errors("> Use `a\n> [example](placeholder.md)\n> b`."), []
        )

    def test_backticks_in_separate_destinations_do_not_hide_a_real_link(self):
        errors = self.errors("[one](missing`one.md) [two](missing`two.md)")
        self.assertEqual(len(errors), 2)

    def test_noninitial_ordered_markers_do_not_interrupt_a_paragraph(self):
        # CommonMark only lets an ordered list starting at 1 interrupt prose.
        for marker in ("2.", "0)"):
            with self.subTest(marker=marker):
                self.assertEqual(
                    self.errors(f"Use `a\n{marker} [example](missing.md)\nb`."), []
                )

    def test_indented_markers_remain_paragraph_continuations(self):
        # Four spaces cannot introduce a heading/list interrupting this paragraph.
        for marker in ("#", "-"):
            with self.subTest(marker=marker):
                self.assertEqual(
                    self.errors(f"Use `a\n    {marker} [example](missing.md)\nb`."), []
                )


if __name__ == "__main__":
    unittest.main()
