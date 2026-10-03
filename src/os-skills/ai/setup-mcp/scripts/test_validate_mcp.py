#!/usr/bin/env python3
"""Regression tests for validate_mcp.py merge behavior (stdlib unittest).

Guards the data-loss contract: merge() must never overwrite a config file it
cannot read or understand, and must merge into (not over) a healthy one.

Run from this directory:
    python3 -m unittest test_validate_mcp -v
"""

import contextlib
import io
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import validate_mcp as vm  # noqa: E402

NEW_CONFIG = json.dumps(
    {"mcpServers": {"new": {"command": "x"}}}
)


class MergeTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.config = os.path.join(self.tmp.name, "settings.json")

    def _write_config(self, content):
        if isinstance(content, bytes):
            mode = "wb"
        else:
            mode = "w"
        with open(self.config, mode, encoding=None if mode == "wb" else "utf-8") as fh:
            fh.write(content)

    def _read_config_bytes(self):
        with open(self.config, "rb") as fh:
            return fh.read()

    def _merge(self):
        with contextlib.redirect_stdout(io.StringIO()), \
                contextlib.redirect_stderr(io.StringIO()):
            vm.merge(NEW_CONFIG, self.config)

    def _merge_expecting_abort(self):
        stderr = io.StringIO()
        with contextlib.redirect_stdout(io.StringIO()), \
                contextlib.redirect_stderr(stderr):
            with self.assertRaises(SystemExit) as ctx:
                vm.merge(NEW_CONFIG, self.config)
        self.assertEqual(ctx.exception.code, 1)
        return stderr.getvalue()

    def test_unparseable_config_aborts_and_preserves_file(self):
        """A config that fails JSON parsing aborts the merge; the file is
        byte-for-byte untouched."""
        broken = b'{"mcpServers": {"keep": {"command": "old"}}} NOT JSON {{{'
        self._write_config(broken)
        stderr = self._merge_expecting_abort()
        self.assertIn("Could not parse", stderr)
        self.assertEqual(self._read_config_bytes(), broken)

    def test_non_object_config_aborts_and_preserves_file(self):
        """A config that parses to a non-object (e.g. an array) aborts the
        merge instead of silently resetting it."""
        array = b'[1, 2, 3]'
        self._write_config(array)
        stderr = self._merge_expecting_abort()
        self.assertIn("not an object", stderr)
        self.assertEqual(self._read_config_bytes(), array)

    def test_invalid_utf8_config_aborts_and_preserves_file(self):
        """A config with undecodable bytes aborts the merge instead of
        crashing with a traceback or discarding the file."""
        binary = b'\xff\xfe{"mcpServers": {}}'
        self._write_config(binary)
        self._merge_expecting_abort()
        self.assertEqual(self._read_config_bytes(), binary)

    def test_merge_keeps_existing_servers_and_sections(self):
        """Happy path: new servers are merged into the existing config and
        unrelated servers and sections survive."""
        self._write_config(
            json.dumps(
                {
                    "mcpServers": {"keep": {"command": "old"}},
                    "mcp": {"timeout": 5},
                }
            )
        )
        self._merge()
        with open(self.config, "r", encoding="utf-8") as fh:
            merged = json.load(fh)
        self.assertEqual(
            sorted(merged["mcpServers"]), ["keep", "new"]
        )
        self.assertEqual(merged["mcp"], {"timeout": 5})

    def test_merge_creates_missing_config(self):
        """Merging with no existing config file creates one holding only the
        new servers."""
        self._merge()
        with open(self.config, "r", encoding="utf-8") as fh:
            merged = json.load(fh)
        self.assertEqual(sorted(merged["mcpServers"]), ["new"])


if __name__ == "__main__":
    unittest.main()
