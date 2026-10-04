#!/usr/bin/env python3
"""Regression tests: OpenClaw credential files must stay owner-only.

`apply_config()` writes ~/.openclaw/openclaw.json (which holds the Model
Studio API key in cleartext) and `.bak` through freshly created files, and
`clear_cached_operator_device_auth()` rewrites identity/device-auth.json
(operator tokens). Both used to inherit the caller's umask (0644/0755 dirs),
so every installer run published the key to other local users and silently
widened a hardened 0600 file back to 0644.

Run from this directory:
    python3 -m unittest test_openclaw_config_permissions -v
"""

import contextlib
import importlib.util
import io
import json
import os
import pathlib
import stat
import sys
import tempfile
import types
import unittest


SCRIPT = pathlib.Path(__file__).with_name("install_openclaw.py")


def _load_module():
    spec = importlib.util.spec_from_file_location("install_openclaw", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@unittest.skipUnless(os.name == "posix", "POSIX file modes required")
class OpenClawCredentialModeTests(unittest.TestCase):
    def setUp(self):
        self.module = _load_module()
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)
        self.config = self.root / ".openclaw" / "openclaw.json"

    def _mode(self, path):
        return stat.S_IMODE(path.stat().st_mode)

    def _apply(self, api_key):
        config = {"models": {"providers": {"bailian": {"apiKey": api_key}}}}
        with contextlib.redirect_stdout(io.StringIO()):
            self.module.apply_config(config, self.config)

    def test_new_config_is_owner_only(self):
        self._apply("sk-first-key")
        self.assertEqual(self._mode(self.config), 0o600)
        self.assertIn("sk-first-key", self.config.read_text(encoding="utf-8"))

    def test_rerun_keeps_owner_only_and_backup_private(self):
        self._apply("sk-first-key")
        os.chmod(self.config, 0o600)

        self._apply("sk-second-key")

        self.assertEqual(self._mode(self.config), 0o600)
        backup = self.config.with_name(self.config.name + ".bak")
        self.assertTrue(backup.exists())
        self.assertEqual(self._mode(backup), 0o600)
        self.assertIn("sk-first-key", backup.read_text(encoding="utf-8"))

    def test_device_auth_rewrite_stays_private(self):
        identity = self.root / ".openclaw" / "identity"
        identity.mkdir(parents=True)
        device_auth = identity / "device-auth.json"
        device_auth.write_text(
            json.dumps({"tokens": {"operator": {"scopes": ["operator.read"]}}}),
            encoding="utf-8",
        )
        os.chmod(device_auth, 0o600)

        args = types.SimpleNamespace(config=str(self.config))
        with contextlib.redirect_stdout(io.StringIO()):
            self.module.clear_cached_operator_device_auth(args)

        self.assertEqual(self._mode(device_auth), 0o600)
        self.assertNotIn("operator", json.loads(device_auth.read_text(encoding="utf-8"))["tokens"])


if __name__ == "__main__":
    unittest.main()
