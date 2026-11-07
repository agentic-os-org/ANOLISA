"""Merging must not overwrite a config that could not be read."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('validate_mcp.py')
NEW = {'mcpServers': {'new': {'command': 'new-tool'}}}


class MergePreservationTests(unittest.TestCase):
    def run_merge(self, path):
        return subprocess.run(
            [sys.executable, str(SCRIPT), json.dumps(NEW), '--merge', str(path)],
            capture_output=True, text=True, encoding='utf-8', timeout=30,
        )

    def test_malformed_existing_config_is_unchanged(self):
        before = b'{"mcpServers":{"keep":{"command":"old"}},"mcp":{"timeout":30},}'
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'config.json'
            path.write_bytes(before)
            result = self.run_merge(path)
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn('ERROR:', result.stderr)
            self.assertNotIn('Written to:', result.stderr)
            self.assertEqual(path.read_bytes(), before)
            self.assertEqual(result.stdout, '')

    def test_undecodable_existing_config_is_unchanged(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'config.json'
            before = b'\xff\xfeexisting content'
            path.write_bytes(before)
            result = self.run_merge(path)
            self.assertEqual(result.returncode, 1)
            self.assertIn('ERROR:', result.stderr)
            self.assertNotIn('Traceback', result.stderr)
            self.assertEqual(path.read_bytes(), before)

    def test_read_error_aborts_before_write(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'config.json'
            path.mkdir()
            result = self.run_merge(path)
            self.assertEqual(result.returncode, 1)
            self.assertIn('ERROR:', result.stderr)
            self.assertNotIn('Traceback', result.stderr)
            self.assertTrue(path.is_dir())

    def test_valid_config_preserves_existing_settings(self):
        before = {'mcpServers': {'keep': {'command': 'old'}},
                  'mcp': {'timeout': 30}, 'other': {'keep': True}}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'config.json'
            path.write_text(json.dumps(before), encoding='utf-8')
            result = self.run_merge(path)
            self.assertEqual(result.returncode, 0, result.stderr)
            expected = dict(before, mcpServers={**before['mcpServers'], **NEW['mcpServers']})
            self.assertEqual(json.loads(path.read_text()), expected)

    def test_empty_existing_config_is_treated_as_new(self):
        for before in (b'', b'   \n'):
            with self.subTest(before=before):
                with tempfile.TemporaryDirectory() as directory:
                    path = Path(directory) / 'config.json'
                    path.write_bytes(before)
                    result = self.run_merge(path)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertEqual(path.read_text(encoding='utf-8').strip(),
                                     json.dumps(NEW, indent=2, ensure_ascii=False))

    def test_non_object_existing_config_is_unchanged(self):
        for before in (b'[]', b'"str"', b'null'):
            with self.subTest(before=before):
                with tempfile.TemporaryDirectory() as directory:
                    path = Path(directory) / 'config.json'
                    path.write_bytes(before)
                    result = self.run_merge(path)
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    self.assertIn('non-object config', result.stderr)
                    self.assertNotIn('Written to:', result.stderr)
                    self.assertEqual(path.read_bytes(), before)

    def test_missing_config_is_created(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'new' / 'config.json'
            result = self.run_merge(path)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(path.read_text()), NEW)


if __name__ == '__main__':
    unittest.main()
