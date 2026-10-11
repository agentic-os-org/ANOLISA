#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Regression tests for shared_strings_builder.py stdout encoding.

The generated sharedStrings.xml declares encoding="UTF-8", but the script
printed it through the process's locale-dependent stdout. On hosts running
a C/POSIX locale (containers, cron, minimal images) any non-ASCII string
crashed the run with UnicodeEncodeError mid-output, leaving a truncated
XML file behind the shell redirection. The index-table output has the
same failure.

The tests pin the C-locale condition (ASCII stdout) with LC_ALL/LANG and
PYTHONIOENCODING so they fail deterministically on every platform and
Python build instead of depending on the runner's default locale.

Baseline on unchanged main: 3 failures / 2 passing controls.
"""

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src"
    / "os-skills"
    / "others"
    / "xlsx"
    / "scripts"
    / "shared_strings_builder.py"
)


def run_cli(args):
    """Run the builder with stdout pinned to the C-locale (ASCII) condition."""
    env = dict(os.environ)
    env["LC_ALL"] = "C"
    env["LANG"] = "C"
    env["PYTHONIOENCODING"] = "ascii"
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args],
        capture_output=True,
        timeout=60,
        env=env,
    )


class SharedStringsUtf8Tests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)

    def test_xml_output_is_utf8_for_non_ascii_strings(self):
        proc = run_cli(["中文标签", "Résumé"])
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn("中文标签".encode("utf-8"), proc.stdout)
        self.assertIn("Résumé".encode("utf-8"), proc.stdout)
        self.assertIn(b'encoding="UTF-8"', proc.stdout)
        self.assertIn(b'count="2"', proc.stdout)
        self.assertIn(b'uniqueCount="2"', proc.stdout)

    def test_index_table_is_utf8_for_non_ascii_strings(self):
        proc = run_cli(["--index", "中文标签"])
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn("中文标签".encode("utf-8"), proc.stdout)

    def test_file_input_is_utf8_for_non_ascii_lines(self):
        strings = Path(self.tmp.name) / "strings.txt"
        strings.write_text("中文标签\n第二个\n", encoding="utf-8")
        proc = run_cli(["--file", str(strings)])
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn("中文标签".encode("utf-8"), proc.stdout)
        self.assertIn("第二个".encode("utf-8"), proc.stdout)

    def test_ascii_strings_unchanged_in_c_locale(self):
        proc = run_cli(["Revenue", "Cost"])
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn(b"<si><t>Revenue</t></si>", proc.stdout)
        self.assertIn(b"<si><t>Cost</t></si>", proc.stdout)

    def test_ascii_index_table_unchanged_in_c_locale(self):
        proc = run_cli(["--index", "Revenue", "Cost"])
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn(b"Revenue", proc.stdout)
        self.assertIn(b"Total: 2 unique strings", proc.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2, exit=False)
