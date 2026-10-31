"""Markdown URL destinations resolve to local decoded paths."""

import importlib.util
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/docs-link-check.py"
SPEC = importlib.util.spec_from_file_location("docs_link_urls", SCRIPT)
CHECKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)


class DestinationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.document = self.root / "README.md"
        for name in ("guide with spaces.md", "指南.md", "guide.md", "literal%20.md"):
            (self.root / name).write_text("target\n", encoding="utf-8")

    def check(self, text: str) -> list[str]:
        self.document.write_text(text, encoding="utf-8")
        return CHECKER.check_file(self.document, self.root)

    def test_encoded_paths_and_queries_resolve(self) -> None:
        for target in (
            "guide%20with%20spaces.md",
            "%E6%8C%87%E5%8D%97.md",
            "guide.md?view=1#intro",
            "literal%2520.md",
        ):
            with self.subTest(target=target):
                self.assertEqual(self.check(f"[guide]({target})"), [])

    def test_angle_brackets_validate_local_destinations(self) -> None:
        self.assertEqual(self.check("[guide](<guide with spaces.md>)"), [])
        self.assertEqual(len(self.check("[missing](<missing file.md>)")), 1)

    def test_external_schemes_and_fragments_are_ignored(self) -> None:
        self.assertEqual(
            self.check(
                "[web](HTTPS://example.com/a) [mail](mailto:user@example.com) [cdn](//example.com/a) [section](#intro)"
            ),
            [],
        )

    def test_missing_encoded_destination_preserves_original_diagnostic(self) -> None:
        errors = self.check("![image](missing%20image.png?raw=1#part)")
        self.assertEqual(len(errors), 1)
        self.assertIn("missing%20image.png?raw=1#part", errors[0])


if __name__ == "__main__":
    unittest.main()
