"""Packing must not overwrite or recursively include source files."""

import os
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts/xlsx_pack.py"


class PackOutputTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "source"
        self.source.mkdir()
        self.manifest = self.source / "[Content_Types].xml"
        self.manifest.write_bytes(
            b'<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>'
        )

    def pack(self, output: Path) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(SCRIPT), str(self.source), str(output)],
            capture_output=True,
            encoding="utf-8",
            env={**os.environ, "PYTHONIOENCODING": "utf-8"},
            timeout=10,
        )

    def test_nested_output_is_rejected_without_creation(self) -> None:
        for relative in ("result.xlsx", "nested/result.xlsx", "nested/../result.xlsx"):
            with self.subTest(relative=relative):
                output = self.source / relative
                output.parent.mkdir(exist_ok=True)
                result = self.pack(output)
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("outside", result.stderr)
                self.assertFalse(output.exists())

    def test_existing_source_part_is_not_truncated(self) -> None:
        original = self.manifest.read_bytes()
        result = self.pack(self.manifest)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(self.manifest.read_bytes(), original)

    def test_sibling_with_shared_prefix_is_valid(self) -> None:
        output = self.root / "source-result.xlsx"
        result = self.pack(output)
        self.assertEqual(result.returncode, 0, result.stderr)
        with zipfile.ZipFile(output) as archive:
            self.assertEqual(archive.namelist(), [self.manifest.name])
            self.assertEqual(archive.read(self.manifest.name), self.manifest.read_bytes())
            self.assertIsNone(archive.testzip())

    def test_output_symlink_to_source_part_is_rejected(self) -> None:
        output = self.root / "linked.xlsx"
        try:
            output.symlink_to(self.manifest)
        except (OSError, NotImplementedError) as exc:
            self.skipTest(f"Symlink creation unavailable: {exc}")
        original = self.manifest.read_bytes()
        result = self.pack(output)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(self.manifest.read_bytes(), original)
        self.assertTrue(output.is_symlink())


if __name__ == "__main__":
    unittest.main(verbosity=2)
