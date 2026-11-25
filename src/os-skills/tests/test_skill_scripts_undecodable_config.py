"""The MCP config helper must report an unreadable config, not traceback on it.

`validate_mcp.py` reads an MCP config in text mode and handles a bad file on
purpose: it reports it (`merge`, `check`). A non-UTF-8 file raises
`UnicodeDecodeError`, which is neither `OSError` nor `json.JSONDecodeError`, so
those handlers were skipped and the user saw a traceback instead of the message
the handler exists to print.

`merge` is the reader whose fallback is followed by a write to the same path,
so for it "handle the bad file" cannot mean "start from empty": that
deletes every setting the file held that is not `mcpServers`. It fails closed
instead, and the tests below pin the file's bytes rather than just the message,
because the message was never the part that could lose data.

Discovered by `.github/workflows/os-skills-prebuilt-ci.yaml`, which triggers on
`src/os-skills/**`. Underscored filename: `unittest discover` silently skips
modules whose name contains a hyphen.
"""

from __future__ import annotations

import importlib.util
import io
import json
import stat
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

SKILLS = Path(__file__).resolve().parents[1]

CORRUPT = b'{"mcpServers": {"x": "\xff"}}'


def _load(relative: str, name: str):
    spec = importlib.util.spec_from_file_location(name, SKILLS / relative)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class ValidateMcpDecodeTest(unittest.TestCase):
    def setUp(self) -> None:
        self.module = _load("ai/setup-mcp/scripts/validate_mcp.py", "validate_mcp_undecodable")
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.config = Path(temporary.name) / "mcp.json"
        self.config.write_bytes(CORRUPT)

    def test_merge_refuses_an_undecodable_config(self) -> None:
        # A merge rewrites the whole file, so falling back to an empty config
        # would drop every setting that is not mcpServers. Measured before the
        # fix: exit 0, and the rewritten file held only the new server.
        before = self.config.read_bytes()
        stderr = io.StringIO()
        with redirect_stderr(stderr), self.assertRaises(SystemExit) as raised:
            self.module.merge('{"mcpServers": {"server": {}}}', str(self.config))

        message = stderr.getvalue()
        self.assertIn("Cannot decode", message)
        self.assertIn(str(self.config), message)
        self.assertNotIsInstance(raised.exception, UnicodeDecodeError)
        self.assertEqual(self.config.read_bytes(), before, "the config must be untouched")

    def test_merge_keeps_the_original_before_replacing_malformed_json(self) -> None:
        # Malformed-but-decodable JSON is recoverable, so the merge still runs;
        # the bytes it replaces must survive in .bak or the settings are gone.
        original = b'{"theme": "dark", "mcpServers": {"existing-server": {"command": "npx"}}, }'
        self.config.write_bytes(original)
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            self.module.merge('{"mcpServers": {"new-server": {}}}', str(self.config))

        self.assertIn("WARNING: Could not parse", stderr.getvalue())
        backup = self.config.with_name(self.config.name + ".bak")
        self.assertEqual(backup.read_bytes(), original, "the replaced bytes must be recoverable")
        rewritten = json.loads(self.config.read_text(encoding="utf-8"))
        self.assertEqual(sorted(rewritten["mcpServers"]), ["new-server"])

    def test_merge_refuses_non_object_config_roots(self) -> None:
        roots = (
            b' [{"a": 1, "keep": "value"}]\n',
            b"[]",
            '"保留内容"'.encode("utf-8"),
            b'""',
            b"123",
            b"1.5",
            b"true",
            b"false",
            b"null",
        )
        for original in roots:
            for mode in (0o600, 0o640):
                with self.subTest(root=original, mode=oct(mode)):
                    self.config.write_bytes(original)
                    self.config.chmod(mode)
                    before = self.config.stat()
                    result = subprocess.run(
                        [sys.executable, self.module.__file__,
                         '{"mcpServers": {"n": {"command": "x"}}}',
                         "--merge", str(self.config)],
                        capture_output=True, text=True, encoding="utf-8",
                    )
                    self.assertEqual(result.returncode, 1, result.stderr)
                    self.assertIn("ERROR: Config in", result.stderr)
                    self.assertIn("is not a JSON object", result.stderr)
                    self.assertIn(str(self.config), result.stderr)
                    self.assertNotIn("Traceback", result.stderr)
                    self.assertEqual(result.stdout, "")
                    self.assertEqual(self.config.read_bytes(), original)
                    after = self.config.stat()
                    self.assertEqual(stat.S_IMODE(after.st_mode), mode)
                    self.assertEqual(after.st_ino, before.st_ino)
                    self.assertEqual(after.st_mtime_ns, before.st_mtime_ns)
                    self.assertEqual(
                        sorted(p.name for p in self.config.parent.iterdir()),
                        ["mcp.json"],
                    )

    def test_merge_non_object_rejection_preserves_symlink_and_backup(self) -> None:
        original = b'[{"keep": "value"}]'
        self.config.write_bytes(original)
        self.config.chmod(0o600)
        link = self.config.parent / "settings.json"
        link.symlink_to(self.config)
        backup = link.with_name(link.name + ".bak")
        backup.write_bytes(b"previous backup")
        stderr = io.StringIO()
        with redirect_stderr(stderr), self.assertRaises(SystemExit) as raised:
            self.module.merge('{"mcpServers": {"n": {"command": "x"}}}', str(link))

        self.assertEqual(raised.exception.code, 1)
        self.assertIn(str(link), stderr.getvalue())
        self.assertTrue(link.is_symlink())
        self.assertEqual(link.resolve(), self.config.resolve())
        self.assertEqual(self.config.read_bytes(), original)
        self.assertEqual(stat.S_IMODE(self.config.stat().st_mode), 0o600)
        self.assertEqual(backup.read_bytes(), b"previous backup")
        self.assertEqual(
            sorted(p.name for p in self.config.parent.iterdir()),
            ["mcp.json", "settings.json", "settings.json.bak"],
        )

    def test_merge_preserves_every_other_setting(self) -> None:
        # The clean-path control: a readable config keeps what it had.
        self.config.write_text(
            '{"theme": "dark", "mcpServers": {"existing-server": {"command": "npx"}}}',
            encoding="utf-8",
        )
        with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
            self.module.merge('{"mcpServers": {"new-server": {"command": "uvx"}}}', str(self.config))

        merged = json.loads(self.config.read_text(encoding="utf-8"))
        self.assertEqual(merged["theme"], "dark")
        self.assertEqual(sorted(merged["mcpServers"]), ["existing-server", "new-server"])
        # The staging file is renamed, not left beside the config.
        self.assertEqual(sorted(p.name for p in self.config.parent.iterdir()), ["mcp.json"])

    def test_merge_preserves_the_file_mode(self) -> None:
        # The rename publishes the staging file's mode, and a fresh staging
        # file is `0666 & ~umask`: without an explicit chmod every merge
        # collapsed 0600 to 0644. This config holds `headers`/`env` values, so
        # that is a security regression, not a cosmetic one.
        for mode in (0o600, 0o640):
            with self.subTest(mode=oct(mode)):
                self.config.write_text('{"theme": "dark"}', encoding="utf-8")
                self.config.chmod(mode)
                with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                    self.module.merge('{"mcpServers": {"s": {}}}', str(self.config))
                self.assertEqual(stat.S_IMODE(self.config.stat().st_mode), mode)

    def test_merge_writes_through_a_symlink(self) -> None:
        # Renaming onto the link would replace it with a regular file and
        # leave the dotfile it pointed at untouched.
        real = self.config.parent / "real.json"
        real.write_text('{"theme": "dark"}', encoding="utf-8")
        link = self.config.parent / "settings.json"
        link.symlink_to(real)
        with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
            self.module.merge('{"mcpServers": {"s": {}}}', str(link))

        self.assertTrue(link.is_symlink(), "the symlink must survive the merge")
        self.assertEqual(json.loads(real.read_text(encoding="utf-8"))["theme"], "dark")

    def test_check_reports_invalid_json(self) -> None:
        stderr = io.StringIO()
        with redirect_stderr(stderr), self.assertRaises(SystemExit) as raised:
            self.module.check(str(self.config))
        self.assertIn("Invalid JSON", stderr.getvalue())
        self.assertNotIsInstance(raised.exception, UnicodeDecodeError)


if __name__ == "__main__":
    unittest.main()
