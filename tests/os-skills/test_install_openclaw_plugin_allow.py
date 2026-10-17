#!/usr/bin/env python3
"""Regression tests for OpenClaw plugin allowlist validation (#7208).

Loads ``install_openclaw.py`` directly (stdlib only, no network, no
OpenClaw install) and drives ``apply_config`` / ``merge_plugin_allow``
against temporary config files.

Baseline on unchanged main: the malformed shapes below raise a raw
``AttributeError`` (``"plugins": null``) or silently write a corrupt
allowlist (a string ``plugins.allow`` unpacked into single characters),
and the two merge controls already pass.

A non-object top-level config value is covered by #3630 and is
deliberately not asserted here.
"""

import importlib.util
import json
import pathlib
import shutil
import tempfile
import unittest

SCRIPT = (
    pathlib.Path(__file__).resolve().parents[2]
    / "src"
    / "os-skills"
    / "ai"
    / "install-openclaw"
    / "scripts"
    / "install_openclaw.py"
)

DINGTALK_CONFIG = {
    "plugins": {
        "enabled": True,
        "allow": ["dingtalk"],
        "entries": {"dingtalk": {"enabled": True}},
    },
    "channels": {"dingtalk": {"enabled": True}},
}
NO_PLUGIN_CONFIG = {"gateway": {"bind": "loopback"}}


def load_installer():
    spec = importlib.util.spec_from_file_location("install_openclaw", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


ioc = load_installer()


class ApplyConfigTestCase(unittest.TestCase):
    def setUp(self):
        self.tmp = pathlib.Path(tempfile.mkdtemp(prefix="openclaw-cfg-"))
        self.config_path = self.tmp / "openclaw.json"
        self.backup_path = self.config_path.with_name(self.config_path.name + ".bak")

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def write_existing(self, text):
        self.config_path.write_text(text, encoding="utf-8")
        return text

    def apply(self, new_config, **kwargs):
        """Run apply_config; return the SystemExit message or None."""
        try:
            ioc.apply_config(new_config, self.config_path, **kwargs)
        except SystemExit as exc:
            return str(exc)
        return None

    def assert_rejected(self, message, *needles):
        self.assertIsNotNone(message, "expected a configuration error")
        for needle in needles:
            self.assertIn(needle, message)
        self.assertIn(str(self.config_path), message)


class TestMalformedExistingPluginsRejected(ApplyConfigTestCase):
    def test_null_plugins_rejected_even_without_new_plugin(self):
        before = self.write_existing('{"plugins": null, "gateway": {"mode": "local"}}')
        message = self.apply(NO_PLUGIN_CONFIG)
        self.assert_rejected(message, "plugins", "JSON object", "null")
        self.assertEqual(self.config_path.read_text(encoding="utf-8"), before)
        self.assertFalse(self.backup_path.exists())

    def test_string_allow_not_unpacked_into_characters(self):
        before = self.write_existing('{"plugins": {"allow": "tokenless"}}')
        message = self.apply(DINGTALK_CONFIG)
        self.assert_rejected(message, "plugins.allow", "JSON array", "a string")
        self.assertEqual(self.config_path.read_text(encoding="utf-8"), before)
        self.assertFalse(self.backup_path.exists())

    def test_list_plugins_rejected(self):
        before = self.write_existing('{"plugins": ["tokenless"]}')
        message = self.apply(DINGTALK_CONFIG)
        self.assert_rejected(message, "plugins", "JSON object", "an array")
        self.assertEqual(self.config_path.read_text(encoding="utf-8"), before)
        self.assertFalse(self.backup_path.exists())

    def test_non_string_allow_entry_rejected(self):
        before = self.write_existing('{"plugins": {"allow": [42, "tokenless"]}}')
        message = self.apply(DINGTALK_CONFIG)
        self.assert_rejected(
            message, "plugins.allow", "plugin name string", "a number"
        )
        self.assertEqual(self.config_path.read_text(encoding="utf-8"), before)
        self.assertFalse(self.backup_path.exists())

    def test_malformed_plugins_rejected_before_backup_is_written(self):
        self.write_existing('{"plugins": null}')
        self.apply(NO_PLUGIN_CONFIG)
        self.assertFalse(self.backup_path.exists())
        self.assertFalse(self.config_path.with_name("openclaw.json.tmp").exists())

    def test_invalid_json_error_is_unchanged(self):
        self.write_existing("{not json")
        message = self.apply(NO_PLUGIN_CONFIG)
        self.assertIsNotNone(message)
        self.assertIn("Invalid JSON", message)

    def test_dry_run_still_does_not_read_the_config(self):
        self.write_existing('{"plugins": {"allow": "tokenless"}}')
        self.assertIsNone(self.apply(NO_PLUGIN_CONFIG, dry_run=True))
        self.assertFalse(self.backup_path.exists())


class TestValidAllowlistMerge(ApplyConfigTestCase):
    def test_existing_entries_keep_order_and_new_plugin_appends(self):
        self.write_existing('{"plugins": {"allow": ["tokenless", "agent-sec"]}}')
        self.assertIsNone(self.apply(DINGTALK_CONFIG))
        written = json.loads(self.config_path.read_text(encoding="utf-8"))
        self.assertEqual(
            written["plugins"]["allow"], ["tokenless", "agent-sec", "dingtalk"]
        )
        self.assertTrue(self.backup_path.exists())

    def test_duplicate_plugin_is_not_repeated(self):
        self.write_existing('{"plugins": {"allow": ["dingtalk", "tokenless"]}}')
        self.assertIsNone(self.apply(DINGTALK_CONFIG))
        written = json.loads(self.config_path.read_text(encoding="utf-8"))
        self.assertEqual(written["plugins"]["allow"], ["dingtalk", "tokenless"])

    def test_empty_allow_array_is_accepted(self):
        self.write_existing('{"plugins": {"enabled": true, "allow": []}}')
        self.assertIsNone(self.apply(DINGTALK_CONFIG))
        written = json.loads(self.config_path.read_text(encoding="utf-8"))
        self.assertEqual(written["plugins"]["allow"], ["dingtalk"])

    def test_missing_plugins_key_is_accepted(self):
        self.write_existing('{"gateway": {"mode": "local"}}')
        self.assertIsNone(self.apply(DINGTALK_CONFIG))
        written = json.loads(self.config_path.read_text(encoding="utf-8"))
        self.assertEqual(written["plugins"]["allow"], ["dingtalk"])

    def test_unrelated_keys_are_still_merged(self):
        self.write_existing('{"gateway": {"mode": "local"}, "plugins": {"allow": []}}')
        combined = {**DINGTALK_CONFIG, "gateway": {"bind": "loopback"}}
        self.assertIsNone(self.apply(combined))
        written = json.loads(self.config_path.read_text(encoding="utf-8"))
        self.assertEqual(written["gateway"], {"mode": "local", "bind": "loopback"})
        self.assertEqual(written["plugins"]["allow"], ["dingtalk"])


class TestMergePluginAllowHelper(unittest.TestCase):
    def test_malformed_existing_rejected_when_no_plugin_requested(self):
        for existing in (
            {"plugins": None},
            {"plugins": ["tokenless"]},
            {"plugins": {"allow": "tokenless"}},
            {"plugins": {"allow": [None]}},
        ):
            with self.subTest(existing=existing):
                with self.assertRaises(SystemExit):
                    ioc.merge_plugin_allow(existing, dict(NO_PLUGIN_CONFIG))

    def test_malformed_generated_config_rejected(self):
        with self.assertRaises(SystemExit):
            ioc.merge_plugin_allow({}, {"plugins": {"allow": "dingtalk"}})

    def test_valid_merge_returns_ordered_unique_list(self):
        merged = ioc.merge_plugin_allow(
            {"plugins": {"allow": ["tokenless", "dingtalk"]}},
            {"plugins": {"allow": ["dingtalk", "agent-sec"]}},
        )
        self.assertEqual(
            merged["plugins"]["allow"], ["tokenless", "dingtalk", "agent-sec"]
        )

    def test_config_without_allow_is_returned_unchanged(self):
        config = {"gateway": {"bind": "loopback"}}
        merged = ioc.merge_plugin_allow({"plugins": {"allow": ["a"]}}, config)
        self.assertEqual(merged, config)


if __name__ == "__main__":
    unittest.main(verbosity=2, exit=False)
