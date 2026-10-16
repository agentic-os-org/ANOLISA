#!/usr/bin/env python3
"""Tests for the credential file modes written by install_openclaw.py (#4901).

The installer publishes ``~/.openclaw/openclaw.json`` (Model Studio API key in
cleartext) and rewrites ``identity/device-auth.json`` (operator tokens) through
a freshly created ``.tmp`` file plus ``os.replace``, and keeps a ``.bak`` of
whatever it replaces. Neither the temp inode nor the backup had a mode set, so
all of them inherited the caller's umask: 0644 under the default 022, readable
by every local user. Because ``os.replace`` publishes the temp inode's mode, a
re-run also downgraded a user-hardened 0600 config back to 0644.

Baseline on unchanged main: the four mode assertions fail with 0644 (420 != 384)
and the helper contract test errors out - the helper does not exist yet.
"""

import argparse
import contextlib
import importlib.util
import io
import json
import os
import stat
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "install_openclaw.py"

API_KEY = "sk-test-placeholder-not-a-real-key"
OLD_API_KEY = "sk-rotated-placeholder-not-a-real-key"
OPERATOR_TOKEN = "operator-token-placeholder-not-a-real-token"


def load_installer():
    spec = importlib.util.spec_from_file_location("install_openclaw", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def mode_of(path):
    return stat.S_IMODE(path.stat().st_mode)


def config_payload(api_key):
    return {
        "models": {
            "providers": {
                "bailian": {
                    "apiKey": api_key,
                    "baseUrl": "https://dashscope.aliyuncs.com/apps/anthropic",
                }
            }
        }
    }


@unittest.skipUnless(os.name == "posix", "POSIX file modes are required")
class OpenClawConfigPermissionsTestCase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.installer = load_installer()

    def setUp(self):
        # 022 is the default umask on the target OS; force it so the assertions
        # do not depend on the umask the suite happens to run under.
        self._saved_umask = os.umask(0o022)

    def tearDown(self):
        os.umask(self._saved_umask)

    def apply_config(self, config_path, api_key=API_KEY):
        with contextlib.redirect_stdout(io.StringIO()):
            self.installer.apply_config(config_payload(api_key), config_path)

    def test_new_config_is_owner_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            config_path = Path(tmp) / "openclaw.json"
            self.apply_config(config_path)

            self.assertTrue(config_path.is_file())
            self.assertIn(API_KEY, config_path.read_text(encoding="utf-8"))
            self.assertEqual(
                mode_of(config_path),
                0o600,
                "openclaw.json holds the API key in cleartext and must be owner-only",
            )
            self.assertEqual(
                list(Path(tmp).glob("*.tmp")), [], "the temp inode must be published, not left behind"
            )

    def test_rerun_keeps_owner_only_and_backup_private(self):
        with tempfile.TemporaryDirectory() as tmp:
            config_path = Path(tmp) / "openclaw.json"

            # A config the user already hardened must survive a re-run.
            config_path.write_text(json.dumps(config_payload(OLD_API_KEY)), encoding="utf-8")
            config_path.chmod(0o600)
            self.apply_config(config_path)

            backup_path = config_path.with_name(config_path.name + ".bak")
            self.assertTrue(backup_path.is_file())
            self.assertIn(OLD_API_KEY, backup_path.read_text(encoding="utf-8"))
            self.assertEqual(
                mode_of(config_path),
                0o600,
                "os.replace() must not downgrade a hardened config back to the umask default",
            )
            self.assertEqual(
                mode_of(backup_path),
                0o600,
                "the backup retains the previous API key and must be owner-only",
            )

            # A world-readable config left behind by an older run gets tightened.
            config_path.chmod(0o644)
            backup_path.chmod(0o644)
            self.apply_config(config_path)
            self.assertEqual(mode_of(config_path), 0o600)
            self.assertEqual(mode_of(backup_path), 0o600)

    def write_device_auth(self, tmp, mode=0o644):
        path = Path(tmp) / "identity" / "device-auth.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            json.dumps(
                {
                    "role": "operator",
                    "tokens": {
                        "operator": {
                            "accessToken": OPERATOR_TOKEN,
                            "scopes": ["operator.read"],
                        }
                    },
                }
            ),
            encoding="utf-8",
        )
        path.chmod(mode)
        return path

    def test_device_auth_rewrite_stays_private(self):
        with tempfile.TemporaryDirectory() as tmp:
            config_path = Path(tmp) / "openclaw.json"
            device_auth = self.write_device_auth(tmp)
            args = argparse.Namespace(config=str(config_path))

            with contextlib.redirect_stdout(io.StringIO()):
                self.installer.clear_cached_operator_device_auth(args)

            self.assertTrue(device_auth.is_file())
            data = json.loads(device_auth.read_text(encoding="utf-8"))
            self.assertNotIn("operator", data["tokens"])
            self.assertEqual(
                mode_of(device_auth),
                0o600,
                "device-auth.json holds operator tokens and must be owner-only",
            )
            self.assertEqual(list(device_auth.parent.glob("*.tmp")), [])

    def test_unreadable_device_auth_backup_stays_private(self):
        with tempfile.TemporaryDirectory() as tmp:
            config_path = Path(tmp) / "openclaw.json"
            device_auth = Path(tmp) / "identity" / "device-auth.json"
            device_auth.parent.mkdir(parents=True, exist_ok=True)
            device_auth.write_text("NOT JSON {{{", encoding="utf-8")
            device_auth.chmod(0o644)
            args = argparse.Namespace(config=str(config_path))

            with contextlib.redirect_stdout(io.StringIO()):
                self.installer.clear_cached_operator_device_auth(args)

            backup = device_auth.with_name(device_auth.name + ".bak")
            self.assertFalse(device_auth.exists(), "the unreadable cache is dropped")
            self.assertTrue(backup.is_file(), "its bytes are kept for recovery")
            self.assertEqual(
                mode_of(backup),
                0o600,
                "the retained bytes still carry the operator tokens",
            )

    def test_restrict_to_owner_is_best_effort(self):
        with tempfile.TemporaryDirectory() as tmp:
            missing = Path(tmp) / "does-not-exist"
            # A filesystem without POSIX modes (or a vanished path) must not
            # fail the install.
            self.installer.restrict_to_owner(missing)

            target = Path(tmp) / "openclaw.json"
            target.write_text("{}", encoding="utf-8")
            target.chmod(0o644)
            self.installer.restrict_to_owner(target)
            self.assertEqual(mode_of(target), 0o600)


if __name__ == "__main__":
    unittest.main()
