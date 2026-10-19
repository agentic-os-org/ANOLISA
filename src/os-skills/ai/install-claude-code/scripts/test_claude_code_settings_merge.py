#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
r"""Tests for the settings.json write in install-claude-code.sh.

Regression tests for #5504, two defects in one heredoc:

1. `write_config()` replaced `~/.claude/settings.json` with a document holding
   only the four `env` keys. A timestamped `.bak` was left behind, but Claude
   Code runs on the wiped file: permissions, hooks, statusLine and the rest of
   the user's configuration stopped applying.
2. The API key was interpolated into that JSON heredoc unescaped, so a key
   containing `"` or `\` produced a document Claude Code refuses to load.

The installer is driven end-to-end with `curl` and `rpm` stubbed on PATH (the
curl stub is the "installer": it drops a fake `claude` binary in
`$HOME/.local/bin`), so the run needs no network, no package manager and no
root.

Usage: python3 src/os-skills/ai/install-claude-code/scripts/test_claude_code_settings_merge.py
"""

import json
import os
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("install-claude-code.sh")

CURL_STUB = """#!/bin/sh
mkdir -p "$HOME/.local/bin"
printf '%s' '#!/bin/sh
echo "fake-claude 0.0.0"
' > "$HOME/.local/bin/claude"
chmod +x "$HOME/.local/bin/claude"
"""

RPM_STUB = """#!/bin/sh
exit 0
"""

EXISTING_SETTINGS = {
    "permissions": {"allow": ["Bash(ls:*)"]},
    "statusLine": {"command": "echo hi"},
    "env": {"KEEP_ME": "yes"},
}

# Valid as a shell argument and as CLAUDE_API_KEY, fatal inside a JSON string.
WEIRD_KEY = 'sk-weird "key" \\ end'

EXPECTED_ENV = {
    "ANTHROPIC_BASE_URL": "https://dashscope.aliyuncs.com/apps/anthropic",
    "ANTHROPIC_MODEL": "qwen3-coder-plus",
    "ANTHROPIC_SMALL_FAST_MODEL": "qwen3-coder-plus",
}


class SettingsMergeTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.stubs = self.root / "bin"
        self.stubs.mkdir()
        for name, body in (("curl", CURL_STUB), ("rpm", RPM_STUB)):
            path = self.stubs / name
            path.write_text(body, encoding="utf-8")
            path.chmod(0o755)
        self.settings = self.home / ".claude" / "settings.json"

    # --- helpers ---------------------------------------------------------

    def run_installer(self, api_key, search_path=None):
        env = dict(os.environ)
        env["HOME"] = str(self.home)
        env["CLAUDE_API_KEY"] = api_key
        path = [str(self.stubs), str(self.home / ".local" / "bin")]
        # Without an explicit path the ambient one applies: the point of the
        # python3-less cases is a host where python3 does not exist at all.
        path.append(search_path if search_path is not None else env["PATH"])
        env["PATH"] = ":".join(path)
        return subprocess.run(
            ["bash", str(SCRIPT), "--config", "--skip-tokenless"],
            env=env,
            capture_output=True,
            text=True,
        )

    def write_settings(self, text):
        self.settings.parent.mkdir(parents=True, exist_ok=True)
        self.settings.write_text(text, encoding="utf-8")

    def read_settings(self):
        return json.loads(self.settings.read_text(encoding="utf-8"))

    def backups(self):
        return sorted(self.settings.parent.glob("settings.json.bak.*"))

    @staticmethod
    def output_of(result):
        # warn()/info() print on stdout, the python helper writes diagnostics on
        # stderr; the message the operator reads is whichever channel it used.
        return result.stdout + result.stderr

    def without_python3(self):
        """Every /usr/bin tool except python* — a host that has no python3."""
        curated = self.root / "no-python3"
        curated.mkdir(exist_ok=True)
        for entry in Path("/usr/bin").iterdir():
            if entry.name.startswith("python"):
                continue
            target = curated / entry.name
            if not target.exists():
                target.symlink_to(entry)
        return str(curated)

    def assert_dashscope_env(self, document, api_key):
        self.assertEqual(document["env"]["ANTHROPIC_AUTH_TOKEN"], api_key)
        for key, value in EXPECTED_ENV.items():
            self.assertEqual(document["env"][key], value)

    # --- the reported defects --------------------------------------------

    def test_existing_settings_are_merged_not_replaced(self):
        self.write_settings(json.dumps(EXISTING_SETTINGS))
        result = self.run_installer("sk-plain")
        self.assertEqual(result.returncode, 0, result.stderr)

        document = self.read_settings()
        self.assertEqual(document["permissions"], EXISTING_SETTINGS["permissions"])
        self.assertEqual(document["statusLine"], EXISTING_SETTINGS["statusLine"])
        self.assertEqual(document["env"]["KEEP_ME"], "yes")
        self.assert_dashscope_env(document, "sk-plain")
        self.assertEqual(len(self.backups()), 1, "the previous file must be backed up")

    def test_api_key_with_quotes_and_backslashes_stays_loadable(self):
        self.write_settings("{}")
        result = self.run_installer(WEIRD_KEY)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_dashscope_env(self.read_settings(), WEIRD_KEY)

    def test_merge_preserves_the_file_mode(self):
        # The token lives in this file; collapsing 0600 to the umask default is
        # a security regression for a file the merge itself just wrote a secret
        # into.
        for mode in (0o600, 0o640):
            with self.subTest(mode=oct(mode)):
                self.write_settings(json.dumps(EXISTING_SETTINGS))
                self.settings.chmod(mode)
                result = self.run_installer("sk-private")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(stat.S_IMODE(os.stat(self.settings).st_mode), mode)
                self.assert_dashscope_env(self.read_settings(), "sk-private")

    def test_api_key_is_not_passed_on_the_command_line(self):
        # /proc/<pid>/cmdline is world-readable; the key belongs in the
        # environment (CWE-214). A logging wrapper proves where it travelled.
        real_python = shutil.which("python3")
        if real_python is None:
            self.skipTest("python3 is required to observe the argv")
        argv_log = self.home / "python-argv.log"
        wrapper = self.stubs / "python3"
        wrapper.write_text(
            "#!/bin/sh\n"
            f'printf "%s\\n" "$@" >> "{argv_log}"\n'
            f'exec "{real_python}" "$@"\n',
            encoding="utf-8",
        )
        wrapper.chmod(0o755)
        self.write_settings("{}")
        result = self.run_installer("sk-super-secret")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("sk-super-secret", argv_log.read_text(encoding="utf-8"),
                         "the API key must not be visible in the process argv")
        self.assert_dashscope_env(self.read_settings(), "sk-super-secret")

    def test_a_fresh_install_creates_the_env_block(self):
        result = self.run_installer("sk-fresh")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_dashscope_env(self.read_settings(), "sk-fresh")
        self.assertEqual(self.backups(), [], "nothing existed, so nothing is backed up")

    # --- a document the script cannot merge into -------------------------

    def test_unparseable_settings_are_left_alone(self):
        broken = '{"permissions": {"allow": ["Bash(ls:*)"],}}\n'
        self.write_settings(broken)
        result = self.run_installer("sk-broken")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.settings.read_text(encoding="utf-8"), broken,
                         "a document the merge cannot read must not be overwritten")
        self.assertIn("untouched", self.output_of(result))
        self.assertEqual(self.backups(), [],
                         "a refused merge changed nothing, so it must not back anything up")

    def test_without_python3_an_existing_file_is_left_alone(self):
        self.write_settings(json.dumps(EXISTING_SETTINGS))
        result = self.run_installer("sk-nopy", search_path=self.without_python3())

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.read_settings(), EXISTING_SETTINGS,
                         "without a JSON parser the four keys cannot be merged, so the file stays")
        self.assertIn("python3 not found", self.output_of(result))
        self.assertEqual(self.backups(), [], "a refused merge must not back anything up")

    def test_without_python3_a_fresh_install_still_escapes_the_key(self):
        result = self.run_installer(WEIRD_KEY, search_path=self.without_python3())
        self.assertEqual(result.returncode, 0, result.stderr)
        # The fallback has no JSON library, so it escapes by hand and the value
        # must still parse back to exactly what the operator typed.
        self.assert_dashscope_env(self.read_settings(), WEIRD_KEY)

    def test_without_python3_a_key_with_control_characters_is_not_written(self):
        # The hand-rolled escaper covers `"` and `\` only; a raw control
        # character would still produce a document Claude Code refuses to load,
        # so the fallback must refuse rather than write one.
        result = self.run_installer("sk-bad\tkey", search_path=self.without_python3())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.settings.exists(),
                         "a key that cannot be escaped must not be written")
        self.assertIn("control characters", self.output_of(result))

    # --- a write that really fails ---------------------------------------

    def test_a_failed_write_is_reported_and_fails_the_install(self):
        if hasattr(os, "geteuid") and os.geteuid() == 0:
            self.skipTest("root ignores the read-only directory")
        self.write_settings("{}")
        self.settings.parent.chmod(0o555)
        try:
            result = self.run_installer("sk-readonly")
        finally:
            self.settings.parent.chmod(0o755)
        self.assertNotEqual(result.returncode, 0,
                            "the caller only sees the exit code, so a lost config must fail it")
        self.assertNotIn("Done! Run 'claude'", result.stdout)
        self.assertIn("Failed to update", self.output_of(result))


if __name__ == "__main__":
    unittest.main(verbosity=2)
