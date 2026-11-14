"""An unreadable OpenClaw config or cache must not escape as a traceback.

`openclaw.json` and the operator caches beside it are the files most likely to
be left half-written by an interrupted install, so `install_openclaw.py` reads
them defensively: the caches fall back to "absent" and the config is reported
as invalid JSON. A text-mode `json.load()` decodes before it parses, so a
non-UTF-8 file raises `UnicodeDecodeError` -- which is neither `OSError` nor
`json.JSONDecodeError`, so those handlers never saw it and the operator got a
bare traceback instead of the message.

Lives under `src/os-skills/tests/` rather than the repository-root suite
because `.github/workflows/os-skills-prebuilt-ci.yaml` discovers this directory
whenever `src/os-skills/**` changes, which any fix in this area does. The
filename uses underscores on purpose: `unittest discover` skips a module whose
name contains a hyphen without reporting anything.
"""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

SCRIPT = (
    Path(__file__).resolve().parents[1]
    / "ai"
    / "install-openclaw"
    / "scripts"
    / "install_openclaw.py"
)


def _load_installer():
    spec = importlib.util.spec_from_file_location("install_openclaw_undecodable", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class UndecodableCacheTest(unittest.TestCase):
    """The caches are treated as absent, not as a crash."""

    def setUp(self) -> None:
        self.module = _load_installer()
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.paired = self.root / "devices" / "paired.json"
        self.device_auth = self.root / "identity" / "device-auth.json"
        self.paired.parent.mkdir()
        self.device_auth.parent.mkdir()
        for target in (self.paired, self.device_auth):
            target.write_bytes(b'{"tokens": "\xff"}')
        self.args = SimpleNamespace(config=str(self.root / "config.json"))

    def test_undecodable_caches_fall_back(self) -> None:
        self.assertIsNone(self.module.clear_read_only_operator_pairings(self.args))
        self.assertFalse(self.module.cached_operator_has_write_scope(self.args))
        self.module.clear_cached_operator_device_auth(self.args)

    def test_the_bad_cache_is_moved_aside_not_left_to_be_parsed_again(self) -> None:
        self.module.clear_cached_operator_device_auth(self.args)
        self.assertFalse(self.device_auth.exists())
        self.assertTrue(self.device_auth.with_name("device-auth.json.bak").exists())


class UndecodableConfigTest(unittest.TestCase):
    """The install main path reports the config instead of tracebacking."""

    def setUp(self) -> None:
        self.module = _load_installer()
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def test_apply_config_reports_an_undecodable_config(self) -> None:
        config = self.root / "openclaw.json"
        config.write_bytes(b'{"mcp": {"servers": "\xff"}}')

        with self.assertRaises(SystemExit) as raised:
            self.module.apply_config({"mcp": {"servers": {}}}, config)

        message = str(raised.exception)
        self.assertIn("Invalid JSON in", message)
        self.assertIn(str(config), message)
        # A SystemExit is the contract; an escaping decode error is the defect.
        self.assertNotIsInstance(raised.exception, UnicodeDecodeError)

    def test_a_valid_config_is_still_merged(self) -> None:
        config = self.root / "openclaw.json"
        config.write_text('{"gateway": {"port": 1234}}', encoding="utf-8")

        self.module.apply_config({"gateway": {"mode": "local"}}, config)

        merged = config.read_text(encoding="utf-8")
        self.assertIn('"port": 1234', merged)
        self.assertIn('"mode": "local"', merged)


if __name__ == "__main__":
    unittest.main()
