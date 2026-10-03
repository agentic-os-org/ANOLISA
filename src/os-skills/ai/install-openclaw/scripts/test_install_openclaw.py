#!/usr/bin/env python3
"""Tests for install_openclaw config merge guards."""
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import install_openclaw


class ApplyConfigGuardTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.config_path = Path(self._tmp.name) / "openclaw.json"

    def tearDown(self):
        self._tmp.cleanup()

    def _write(self, text):
        self.config_path.write_text(text, encoding="utf-8")

    def test_non_object_existing_config_exits_cleanly(self):
        self._write("[1, 2]")
        with self.assertRaises(SystemExit) as ctx:
            install_openclaw.apply_config(
                {"models": {"mode": "merge"}}, self.config_path
            )
        self.assertIn("expected a JSON object", str(ctx.exception))

    def test_plugins_list_existing_config_merges_cleanly(self):
        self._write('{"plugins": ["foo"]}')
        install_openclaw.apply_config(
            {"plugins": {"allow": ["dingtalk"]}}, self.config_path
        )
        merged = json.loads(self.config_path.read_text(encoding="utf-8"))
        self.assertEqual(merged["plugins"]["allow"], ["dingtalk"])

    def test_invalid_json_existing_config_still_exits_cleanly(self):
        self._write("{not json")
        with self.assertRaises(SystemExit):
            install_openclaw.apply_config({"models": {}}, self.config_path)

    def test_plugin_allow_lists_are_unioned(self):
        self._write('{"plugins": {"allow": ["a"]}}')
        install_openclaw.apply_config(
            {"plugins": {"allow": ["b", "a"]}}, self.config_path
        )
        merged = json.loads(self.config_path.read_text(encoding="utf-8"))
        self.assertEqual(merged["plugins"]["allow"], ["a", "b"])


class MergePluginAllowGuardTest(unittest.TestCase):
    def test_non_dict_existing_plugins_treated_as_absent(self):
        merged = install_openclaw.merge_plugin_allow(
            {"plugins": ["foo"]}, {"plugins": {"allow": ["dingtalk"]}}
        )
        self.assertEqual(merged["plugins"]["allow"], ["dingtalk"])

    def test_non_dict_merged_plugins_returns_merged_unchanged(self):
        merged = install_openclaw.merge_plugin_allow(
            {"plugins": {"allow": ["a"]}}, {"plugins": ["foo"]}
        )
        self.assertEqual(merged, {"plugins": ["foo"]})

    def test_normal_allow_lists_are_unioned(self):
        merged = install_openclaw.merge_plugin_allow(
            {"plugins": {"allow": ["a"]}}, {"plugins": {"allow": ["b"]}}
        )
        self.assertEqual(merged["plugins"]["allow"], ["a", "b"])


if __name__ == "__main__":
    unittest.main()
