#!/usr/bin/env python3
"""Regression tests for the TOML version reader of check-component-versions."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).resolve().parent / "check-component-versions.py"


def load_module():
    # The script filename is dashed, so it must be imported by location.
    spec = importlib.util.spec_from_file_location("check_component_versions", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ReadTomlVersionTest(unittest.TestCase):
    """read_toml_version trusts only the conventional version sections."""

    def setUp(self) -> None:
        temporary_directory = tempfile.TemporaryDirectory()
        self.addCleanup(temporary_directory.cleanup)
        self.root = Path(temporary_directory.name)
        self.module = load_module()

    def write(self, text: str) -> str:
        path = self.root / "fixture.toml"
        path.write_text(text, encoding="utf-8")
        return str(path)

    def test_reads_version_from_each_conventional_section(self):
        cases = {
            '[package]\nname = "x"\nversion = "1.2.3"\n': "1.2.3",
            '[workspace.package]\nversion = "4.5.6"\n': "4.5.6",
            '[component]\nname = "x"\nversion = "7.8.9"\n': "7.8.9",
            'version = "0.0.1"\n': "0.0.1",
        }
        for text, expected in cases.items():
            with self.subTest(text=text):
                self.assertEqual(self.module.read_toml_version(self.write(text)), expected)

    def test_package_section_takes_precedence(self):
        text = '[workspace.package]\nversion = "9.9.9"\n\n[package]\nversion = "1.0.0"\n'
        self.assertEqual(self.module.read_toml_version(self.write(text)), "1.0.0")

    def test_dependency_versions_do_not_satisfy_the_lookup(self):
        # A file that lost its [workspace.package].version but still declares
        # dependency versions must not pass the gate. Both forms put
        # `version = "..."` on its own line, so the old regex matched them.
        texts = [
            '[workspace.dependencies.clap]\nversion = "1.0"\n',
            '[workspace.dependencies]\nclap = {\n  version = "1.0",\n}\n',
        ]
        for text in texts:
            with self.subTest(text=text):
                with self.assertRaises(ValueError):
                    self.module.read_toml_version(self.write(text))

    def test_malformed_toml_is_rejected(self):
        # Broken metadata must fail the gate, not greenlight it via regex.
        path = self.write('[package]\nversion = "1.2.3"\nthis is not toml {{{')
        with self.assertRaises(ValueError) as context:
            self.module.read_toml_version(path)
        self.assertIn("malformed TOML", str(context.exception))

    def test_missing_version_is_rejected(self):
        path = self.write('[package]\nname = "x"\n')
        with self.assertRaises(ValueError):
            self.module.read_toml_version(path)

    def test_regex_fallback_without_tomllib(self):
        # Python < 3.11 has no tomllib; the regex remains an explicit,
        # best-effort fallback for that environment only.
        with mock.patch.object(self.module, "tomllib", None):
            path = self.write('[component]\nversion = "2.0.0"\n')
            self.assertEqual(self.module.read_toml_version(path), "2.0.0")
            with self.assertRaises(ValueError):
                self.module.read_toml_version(self.write('[component]\nname = "x"\n'))


class LiveContractsTest(unittest.TestCase):
    """Every registered contract still resolves through the new reader."""

    def setUp(self) -> None:
        self.module = load_module()

    def test_all_contracts_resolve_a_version(self):
        for source, target in self.module.TOML_CONTRACTS:
            for path in (source, target):
                with self.subTest(path=path):
                    version = self.module.read_version(path)
                    self.assertRegex(version, r"^\d+\.\d+")


if __name__ == "__main__":
    unittest.main()
