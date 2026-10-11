#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Read known legacy exports deterministically with an explicit codec."""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts/xlsx_reader.py"


class ExplicitEncodingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.work_dir = Path(self.temp_dir.name)

    def run_reader(self, path: Path, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT), str(path), "--json", *arguments],
            capture_output=True,
            text=True,
            encoding="utf-8",
            env=dict(os.environ, PYTHONIOENCODING="utf-8"),
        )

    def test_csv_windows_1252_text_is_preserved(self) -> None:
        path = self.work_dir / "export.csv"
        original = "name,value\ncafé €,1\n".encode("cp1252")
        path.write_bytes(original)
        result = self.run_reader(path, "--encoding", "cp1252")
        self.assertEqual(result.returncode, 0, result.stderr)
        data = json.loads(result.stdout)
        self.assertEqual(data["structure"]["export"]["preview"][0]["name"], "café €")
        self.assertEqual(path.read_bytes(), original)

    def test_tsv_uses_requested_utf16_codec(self) -> None:
        path = self.work_dir / "export.tsv"
        path.write_bytes("name\tvalue\nRésumé\t1\n".encode("utf-16"))
        result = self.run_reader(path, "--encoding", "utf-16")
        self.assertEqual(result.returncode, 0, result.stderr)
        data = json.loads(result.stdout)
        self.assertEqual(data["structure"]["export"]["preview"][0]["name"], "Résumé")

    def test_wrong_explicit_encoding_fails_without_fallback(self) -> None:
        path = self.work_dir / "export.csv"
        path.write_bytes("name\ncafé €\n".encode("cp1252"))
        result = self.run_reader(path, "--encoding", "utf-8")
        self.assertEqual(result.returncode, 1)
        self.assertIn("utf-8", result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertNotIn("Traceback", result.stderr)

    def test_unknown_codec_has_a_clear_error(self) -> None:
        path = self.work_dir / "export.csv"
        path.write_text("name\nAlice\n", encoding="utf-8")
        result = self.run_reader(path, "--encoding", "unknown-codec")
        self.assertEqual(result.returncode, 1)
        self.assertIn("unknown-codec", result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def test_encoding_option_rejected_for_excel(self) -> None:
        path = self.work_dir / "book.xlsx"
        path.touch()
        result = self.run_reader(path, "--encoding", "utf-8")
        self.assertEqual(result.returncode, 1)
        self.assertIn("CSV", result.stderr)
        self.assertIn("TSV", result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def test_default_utf8_detection_is_unchanged(self) -> None:
        path = self.work_dir / "export.csv"
        path.write_text("name,value\nAlice,1\n", encoding="utf-8")
        result = self.run_reader(path)
        self.assertEqual(result.returncode, 0, result.stderr)
        data = json.loads(result.stdout)
        self.assertEqual(data["structure"]["export"]["preview"][0]["name"], "Alice")


if __name__ == "__main__":
    unittest.main()
