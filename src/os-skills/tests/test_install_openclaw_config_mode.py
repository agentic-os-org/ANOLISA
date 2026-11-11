"""Keep the mode of the OpenClaw files that hold credentials.

Three files carry secrets: `openclaw.json` holds `providers.*.apiKey`,
`devices/paired.json` holds pairing scopes, and `identity/device-auth.json`
holds device tokens. Each is rewritten by staging a `.tmp` file and renaming
it over the original, so the mode of the published file is the staging file's
`0666 & ~umask` unless it is set explicitly; every `.bak` has the same problem
when it copies bytes rather than metadata, and for the corrupt-JSON branch the
backup is the only surviving copy. All of this is silent under the default
umask, which is why each shape below is asserted rather than assumed.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import stat
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "ai/install-openclaw/scripts/install_openclaw.py"


def _load():
    spec = importlib.util.spec_from_file_location("install_openclaw_for_test", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class OpenClawCredentialFileModeTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.module = _load()

    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.config = self.directory / "openclaw.json"

    def apply(self, payload) -> None:
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(
            io.StringIO()
        ):
            self.module.apply_config(payload, self.config)

    def mode(self, path: Path) -> int:
        return stat.S_IMODE(path.stat().st_mode)

    def test_apply_config_preserves_the_file_mode(self) -> None:
        for mode in (0o600, 0o640):
            with self.subTest(mode=oct(mode)):
                self.config.write_text(
                    '{"providers": {"p": {"apiKey": "k"}}}', encoding="utf-8"
                )
                self.config.chmod(mode)
                self.apply({"gateway": {"port": 1234}})

                self.assertEqual(self.mode(self.config), mode)
                merged = json.loads(self.config.read_text(encoding="utf-8"))
                self.assertEqual(merged["providers"]["p"]["apiKey"], "k")
                self.assertEqual(merged["gateway"]["port"], 1234)

    def test_apply_config_backup_keeps_the_file_mode(self) -> None:
        self.config.write_text('{"providers": {}}', encoding="utf-8")
        self.config.chmod(0o600)
        self.apply({"gateway": {"port": 1234}})

        backup = self.config.with_name(self.config.name + ".bak")
        self.assertTrue(backup.exists(), "an existing config must be backed up")
        self.assertEqual(self.mode(backup), 0o600)

    def test_apply_config_starts_a_new_file_private(self) -> None:
        self.apply({"providers": {"p": {"apiKey": "k"}}})

        self.assertEqual(self.mode(self.config), 0o600)

    def args_for(self, config: Path):
        """Minimal stand-in for the parsed CLI arguments these helpers take."""
        return type("Args", (), {"config": str(config)})()

    def test_paired_devices_rewrite_keeps_the_file_mode(self) -> None:
        # A read-only operator pairing is pruned by rewriting the file, so the
        # same umask default would have been published over the pairing store.
        devices = self.directory / "devices" / "paired.json"
        devices.parent.mkdir(parents=True)
        devices.write_text(
            json.dumps(
                {
                    "read-only": {"role": "operator", "scopes": ["operator.read"]},
                    "writer": {"role": "operator", "scopes": ["operator.write"]},
                }
            ),
            encoding="utf-8",
        )
        devices.chmod(0o600)
        with contextlib.redirect_stdout(io.StringIO()):
            self.module.clear_read_only_operator_pairings(self.args_for(self.config))

        self.assertEqual(self.mode(devices), 0o600)
        remaining = json.loads(devices.read_text(encoding="utf-8"))
        self.assertEqual(sorted(remaining), ["writer"])

    def test_device_auth_rewrite_keeps_the_file_mode(self) -> None:
        # This file holds device tokens, so the umask default is the widest
        # exposure of the three.
        identity = self.directory / "identity"
        identity.mkdir(parents=True)
        auth = identity / "device-auth.json"
        auth.write_text(
            json.dumps({"tokens": {"operator": {"token": "secret"}, "user": {}}}),
            encoding="utf-8",
        )
        auth.chmod(0o600)
        with contextlib.redirect_stdout(io.StringIO()):
            self.module.clear_cached_operator_device_auth(self.args_for(self.config))

        self.assertEqual(self.mode(auth), 0o600)
        remaining = json.loads(auth.read_text(encoding="utf-8"))
        self.assertEqual(sorted(remaining["tokens"]), ["user"])

    def test_corrupt_device_auth_backup_keeps_the_file_mode(self) -> None:
        # This branch unlinks the original, so the backup is the only copy of
        # the device token left; a umask-default backup would be the only copy
        # and readable by other users.
        identity = self.directory / "identity"
        identity.mkdir(parents=True)
        auth = identity / "device-auth.json"
        auth.write_text("{not json", encoding="utf-8")
        auth.chmod(0o600)
        with contextlib.redirect_stdout(io.StringIO()):
            self.module.clear_cached_operator_device_auth(self.args_for(self.config))

        self.assertFalse(auth.exists(), "an unreadable file is replaced by its backup")
        backup = identity / "device-auth.json.bak"
        self.assertEqual(self.mode(backup), 0o600)
        self.assertEqual(backup.read_text(encoding="utf-8"), "{not json")

    def test_failed_write_leaves_no_staging_file(self) -> None:
        # A `.tmp` that never reaches the rename still holds the bytes already
        # flushed, so a failed write must not leave it behind.
        self.config.write_text('{"providers": {}}', encoding="utf-8")
        self.config.chmod(0o600)
        with self.assertRaises(TypeError):
            self.module.write_json_preserving_mode(
                self.config, {"providers": object()}
            )

        self.assertEqual(
            sorted(path.name for path in self.directory.iterdir()), ["openclaw.json"]
        )
        self.assertEqual(self.mode(self.config), 0o600)

    def test_staging_file_is_never_wider_while_written(self) -> None:
        # Creating the staging file and chmod-ing it afterwards leaves the
        # credentials readable by other users for the length of the write, so
        # its mode at the moment the content is written is what gets asserted.
        self.config.write_text('{"providers": {}}', encoding="utf-8")
        self.config.chmod(0o600)
        seen = []
        staging = self.config.with_name(self.config.name + ".tmp")
        original = self.module.json.dump

        def observe(obj, fh, **kwargs):
            seen.append(stat.S_IMODE(staging.stat().st_mode))
            return original(obj, fh, **kwargs)

        self.module.json.dump = observe
        try:
            self.module.write_json_preserving_mode(self.config, {"providers": {}})
        finally:
            self.module.json.dump = original

        self.assertEqual(seen, [0o600], "staged wider than the file it replaces")

    def test_apply_config_leaves_no_staging_file(self) -> None:
        self.config.write_text('{"providers": {}}', encoding="utf-8")
        self.apply({"gateway": {"port": 1234}})

        self.assertEqual(
            sorted(path.name for path in self.directory.iterdir()),
            ["openclaw.json", "openclaw.json.bak"],
        )


if __name__ == "__main__":
    unittest.main()
