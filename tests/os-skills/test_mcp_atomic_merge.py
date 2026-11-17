#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Failed MCP config publication must preserve the last valid configuration."""

import contextlib
import importlib.util
import io
import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import TextIO
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[2] / "src/os-skills/ai/setup-mcp/scripts/validate_mcp.py"
SPEC = importlib.util.spec_from_file_location("validate_mcp", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
mcp = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(mcp)
NEW = '{"mcpServers":{"new":{"command":"new-command"}}}'
OLD = b'{"mcpServers":{"old":{"command":"old-command"}},"other":true}\n'


class AtomicMergeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.directory = Path(self.temp_dir.name)
        self.config = self.directory / "config.json"
        self.config.write_bytes(OLD)

    def assert_failed_merge_preserves_config(self, new: str = NEW) -> None:
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            with self.assertRaises(SystemExit) as raised:
                mcp.merge(new, str(self.config))
        self.assertEqual(raised.exception.code, 1)
        self.assertEqual(self.config.read_bytes(), OLD)
        self.assertEqual(stdout.getvalue(), "")
        self.assertIn("ERROR", stderr.getvalue())
        self.assertNotIn("Written to:", stderr.getvalue())
        self.assertEqual(list(self.directory.iterdir()), [self.config])

    def test_partial_serialization_failure_preserves_old_json(self) -> None:
        def partial_dump(data: dict, stream: TextIO, **kwargs: object) -> None:
            stream.write('{"partial":')
            raise OSError("simulated write failure")

        with mock.patch.object(mcp.json, "dump", side_effect=partial_dump):
            self.assert_failed_merge_preserves_config()

    def test_replace_failure_preserves_old_json_and_removes_staging_file(self) -> None:
        with mock.patch.object(mcp.os, "replace", side_effect=OSError("simulated replace failure")):
            self.assert_failed_merge_preserves_config()

    def test_flush_failure_does_not_publish_staging_file(self) -> None:
        with mock.patch.object(mcp.os, "fsync", side_effect=OSError("simulated flush failure")):
            self.assert_failed_merge_preserves_config()

    def test_unencodable_json_text_fails_cleanly_without_truncation(self) -> None:
        self.assert_failed_merge_preserves_config('{"mcpServers":{"new":{"command":"\\ud800"}}}')

    @unittest.skipUnless(os.name == "nt", "Windows read-only replacement semantics")
    def test_read_only_target_failure_removes_read_only_staging_file(self) -> None:
        self.config.chmod(stat.S_IREAD)
        self.addCleanup(self.config.chmod, stat.S_IWRITE)
        self.assert_failed_merge_preserves_config()

    def test_success_keeps_existing_settings_and_prints_published_json(self) -> None:
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(io.StringIO()):
            mcp.merge(NEW, str(self.config))
        data = json.loads(self.config.read_bytes())
        self.assertEqual(set(data["mcpServers"]), {"old", "new"})
        self.assertTrue(data["other"])
        self.assertEqual(json.loads(stdout.getvalue()), data)
        self.assertEqual(list(self.directory.iterdir()), [self.config])

    def test_cli_creates_new_parent_and_valid_configuration(self) -> None:
        new_config = self.directory / "nested/config.json"
        result = subprocess.run(
            [sys.executable, str(SCRIPT), NEW, "--merge", str(new_config)],
            capture_output=True,
            encoding="utf-8",
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), json.loads(new_config.read_bytes()))
        self.assertIn("Written to:", result.stderr)

    @unittest.skipIf(os.name == "nt", "POSIX mode bits do not apply on Windows")
    def test_existing_mode_bits_are_preserved(self) -> None:
        self.config.chmod(0o640)
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            mcp.merge(NEW, str(self.config))
        self.assertEqual(stat.S_IMODE(self.config.stat().st_mode), 0o640)

    def test_symlink_updates_target_and_preserves_link(self) -> None:
        link = self.directory / "linked.json"
        try:
            link.symlink_to(self.config)
        except OSError as error:
            self.skipTest(f"Symlinks unavailable: {error}")
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            mcp.merge(NEW, str(link))
        self.assertTrue(link.is_symlink())
        self.assertIn("new", json.loads(self.config.read_bytes())["mcpServers"])


if __name__ == "__main__":
    unittest.main()
