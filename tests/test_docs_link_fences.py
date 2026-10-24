"""Code fence delimiters must not hide prose or expose example links."""

import importlib.util
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/docs-link-check.py"
SPEC = importlib.util.spec_from_file_location("docs_link_fences", SCRIPT)
CHECKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)


class FenceTests(unittest.TestCase):
    def check(self, text: str) -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            document = root / "README.md"
            document.write_text(text, encoding="utf-8")
            return CHECKER.check_file(document, root)

    def test_tilde_and_indented_fences_hide_examples(self) -> None:
        for delimiter in ("~~~", "   ~~~~", " ```"):
            with self.subTest(delimiter=delimiter):
                self.assertEqual(
                    self.check(f"{delimiter}markdown\n[example](missing.md)\n{delimiter}\n"), []
                )

    def test_shorter_and_other_delimiters_do_not_close_a_fence(self) -> None:
        for opening, inner in (("````", "```"), ("~~~", "```"), ("~~~~", "~~~")):
            with self.subTest(opening=opening, inner=inner):
                self.assertEqual(
                    self.check(f"{opening}\n{inner}\n[example](missing.md)\n{opening}\n"), []
                )

    def test_unclosed_fence_runs_to_end_of_document(self) -> None:
        self.assertEqual(self.check("```markdown\n[example](missing.md)\n"), [])

    def test_prose_after_a_closing_fence_is_checked(self) -> None:
        errors = self.check("````markdown\n```\n[example](fake.md)\n````\n[real](absent.md)\n")
        self.assertEqual(len(errors), 1)
        self.assertIn("absent.md", errors[0])

    def test_inline_backticks_do_not_start_a_block_fence(self) -> None:
        self.assertEqual(len(self.check("prefix ``` sample\n\n[real](absent.md)\n\nsuffix ```")), 1)

    def test_fences_inside_quote_and_list_containers_hide_examples(self) -> None:
        for example in (
            "> ```markdown\n> [example](fake.md)\n> ```\n",
            "> > ~~~\n> > [example](fake.md)\n> > ~~~\n",
            "- ```markdown\n  [example](fake.md)\n  ```\n",
            "1. ~~~\n   [example](fake.md)\n   ~~~\n",
            "> - ```\n>   [example](fake.md)\n>   ```\n",
        ):
            with self.subTest(example=example):
                errors = self.check(example + "[real](absent.md)\n")
                self.assertEqual(len(errors), 1)
                self.assertIn("absent.md", errors[0])

    def test_leaving_an_unclosed_container_checks_following_prose(self) -> None:
        for example in (
            "> ```\n> [example](fake.md)\n",
            "- ~~~\n  [example](fake.md)\n",
        ):
            with self.subTest(example=example):
                errors = self.check(example + "[real](absent.md)\n")
                self.assertEqual(len(errors), 1)
                self.assertIn("absent.md", errors[0])

    def test_list_continuation_fence_ends_at_the_list_boundary(self) -> None:
        for example in (
            "- item\n\n  ```markdown\n  [example](fake.md)\n\n",
            "- item\n  - nested\n\n    ~~~\n    [example](fake.md)\n\n",
            "> - item\n>\n>   ```markdown\n>   [example](fake.md)\n",
        ):
            with self.subTest(example=example):
                errors = self.check(example + "[real](absent.md)\n")
                self.assertEqual(len(errors), 1)
                self.assertIn("absent.md", errors[0])

    def test_lazy_list_paragraph_preserves_the_following_fence_container(self) -> None:
        for paragraph in (
            "- item\nlazy continuation\n",
            "- item\n  - nested\nlazy nested continuation\n",
            "- item\n===\n",
            "- item\n<span>inline text\n",
        ):
            with self.subTest(paragraph=paragraph):
                indentation = "    " if "nested" in paragraph else "  "
                errors = self.check(
                    paragraph
                    + f"\n{indentation}```markdown\n{indentation}[example](fake.md)\n\n"
                    + "[real](absent.md)\n"
                )
                self.assertEqual(len(errors), 1)
                self.assertIn("absent.md", errors[0])

    def test_a_new_block_ends_lazy_list_continuation(self) -> None:
        for block in (
            "# heading",
            "---",
            "<div>content",
            "<span>",
            "<!-- comment -->",
            "<?instruction?>",
            "<!DOCTYPE html>",
            "<![CDATA[text]]>",
        ):
            with self.subTest(block=block):
                self.assertEqual(
                    self.check(
                        f"- item\n{block}\n\n  ```markdown\n  [example](fake.md)\n\n"
                        "[also code](absent.md)\n"
                    ),
                    [],
                )

    def test_a_blank_line_ends_lazy_list_continuation(self) -> None:
        self.assertEqual(
            self.check(
                "- item\n\noutside paragraph\n\n  ```markdown\n  [example](fake.md)\n\n"
                "[also code](absent.md)\n"
            ),
            [],
        )


if __name__ == "__main__":
    unittest.main()
