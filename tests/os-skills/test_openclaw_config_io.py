"""Characterize the OpenClaw config merge and persistence contract."""

import contextlib
import copy
import importlib
import importlib.util
import io
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPT_DIR = Path(__file__).resolve().parents[2] / "src/os-skills/ai/install-openclaw/scripts"


def load_installer():
    spec = importlib.util.spec_from_file_location(
        "openclaw_config_test_installer", SCRIPT_DIR / "install_openclaw.py"
    )
    module = importlib.util.module_from_spec(spec)
    original_path = sys.path[:]
    try:
        sys.path.insert(0, str(SCRIPT_DIR))
        spec.loader.exec_module(module)
    finally:
        sys.path[:] = original_path
    return module


original_path = sys.path[:]
try:
    sys.path.insert(0, str(SCRIPT_DIR))
    CONFIG = importlib.import_module("openclaw_config_io")
finally:
    sys.path[:] = original_path
INSTALLER = load_installer()


class OpenClawConfigIOTests(unittest.TestCase):
    def test_installer_preserves_config_helper_exports(self):
        for name in ("deep_merge", "ordered_unique", "merge_plugin_allow", "apply_config"):
            with self.subTest(helper=name):
                self.assertIs(getattr(INSTALLER, name), getattr(CONFIG, name))

    def test_nested_merge_preserves_inputs_and_replaces_lists(self):
        existing = {"gateway": {"port": 18789, "auth": {"mode": "token"}}, "list": [1]}
        incoming = {"gateway": {"auth": {"token": "sample"}}, "list": [2]}
        originals = copy.deepcopy((existing, incoming))
        self.assertEqual(
            CONFIG.deep_merge(existing, incoming),
            {
                "gateway": {"port": 18789, "auth": {"mode": "token", "token": "sample"}},
                "list": [2],
            },
        )
        self.assertEqual((existing, incoming), originals)

    def test_ordered_unique_accepts_iterables_and_unhashable_values(self):
        values = iter(["a", "b", "a", "", None, 0, [1], [1], [2]])
        self.assertEqual(CONFIG.ordered_unique(values), ["a", "b", [1], [2]])

    def test_plugin_allow_preserves_old_order_and_absent_allow(self):
        existing = {"plugins": {"allow": ["old", "common"]}}
        merged = {"plugins": {"allow": ["new", "common", ""], "enabled": True}}
        result = CONFIG.merge_plugin_allow(existing, merged)
        self.assertIs(result, merged)
        self.assertEqual(result["plugins"], {"allow": ["old", "common", "new"], "enabled": True})
        untouched = {"plugins": {"enabled": True}}
        self.assertIs(CONFIG.merge_plugin_allow(existing, untouched), untouched)
        self.assertEqual(untouched, {"plugins": {"enabled": True}})

    def test_new_config_creates_parents_and_preserves_unicode(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "nested" / "openclaw.json"
            incoming = {"agents": {"defaults": {"workspace": "工作空间"}}}
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                CONFIG.apply_config(incoming, path)
            self.assertEqual(json.loads(path.read_text(encoding="utf-8")), incoming)
            self.assertIn("工作空间", path.read_text(encoding="utf-8"))
            self.assertTrue(path.read_text(encoding="utf-8").endswith("\n"))
            self.assertFalse(path.with_name(path.name + ".bak").exists())
            self.assertFalse(path.with_name(path.name + ".tmp").exists())
            self.assertIn(f"Config written: {path}", output.getvalue())

    def test_existing_config_keeps_exact_backup_and_merges_plugin_allow(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "openclaw.json"
            original = (
                b'{\r\n "plugins": {"allow": ["old", "common"]},\r\n'
                b' "gateway": {"port": 18789, "bind": "loopback"}\r\n}\r\n'
            )
            path.write_bytes(original)
            incoming = {"plugins": {"allow": ["common", "new"]}, "gateway": {"port": 18800}}
            with contextlib.redirect_stdout(io.StringIO()):
                CONFIG.apply_config(incoming, path)
            self.assertEqual(path.with_name(path.name + ".bak").read_bytes(), original)
            self.assertEqual(
                json.loads(path.read_text(encoding="utf-8")),
                {
                    "plugins": {"allow": ["old", "common", "new"]},
                    "gateway": {"port": 18800, "bind": "loopback"},
                },
            )
            self.assertFalse(path.with_name(path.name + ".tmp").exists())

    def test_invalid_json_is_reported_before_any_write(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "openclaw.json"
            original = b"{invalid\r\n"
            path.write_bytes(original)
            with contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaisesRegex(SystemExit, "Invalid JSON in") as caught:
                    CONFIG.apply_config({"gateway": {}}, path)
            self.assertIsInstance(caught.exception.__cause__, json.JSONDecodeError)
            self.assertEqual(path.read_bytes(), original)
            self.assertEqual(list(Path(directory).iterdir()), [path])

    def test_dry_run_has_no_io_and_does_not_print_config_values(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "absent" / "openclaw.json"
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                CONFIG.apply_config({"models": {"apiKey": "private-value"}}, path, dry_run=True)
            self.assertEqual(list(Path(directory).iterdir()), [])
            self.assertIn(f"would write OpenClaw config to {path}", output.getvalue())
            self.assertIn("[OK] models", output.getvalue())
            self.assertNotIn("private-value", output.getvalue())

    def test_replace_failure_preserves_original_and_backup(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "openclaw.json"
            original = b'{"gateway": {"port": 18789}}\n'
            path.write_bytes(original)
            config_os = CONFIG.apply_config.__globals__["os"]
            with mock.patch.object(config_os, "replace", side_effect=OSError("test failure")):
                with contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaisesRegex(OSError, "test failure"):
                        CONFIG.apply_config({"gateway": {"port": 18800}}, path)
            self.assertEqual(path.read_bytes(), original)
            self.assertEqual(path.with_name(path.name + ".bak").read_bytes(), original)

    def test_standalone_help_works_from_a_copied_skill_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            copied = Path(directory) / "copied-skill"
            shutil.copytree(SCRIPT_DIR, copied)
            unrelated = Path(directory) / "unrelated"
            unrelated.mkdir()
            result = subprocess.run(
                [sys.executable, str(copied / "install_openclaw.py"), "--help"],
                cwd=unrelated,
                capture_output=True,
                text=True,
                timeout=20,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("--dry-run", result.stdout)


if __name__ == "__main__":
    unittest.main()
