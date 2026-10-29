"""Exercise the shipped uninstaller's host-owned config boundary with a CLI stub."""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(
    os.environ.get(
        "AGENT_MEMORY_UNINSTALL_TEST_SCRIPT",
        str(
            Path(__file__).resolve().parents[1]
            / "adapters/agent-memory/openclaw/scripts/uninstall.sh"
        ),
    )
)


class UninstallTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="memory-uninstall-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.state = self.root / "state"
        self.home = self.root / "home"
        self.state.mkdir()
        self.home.mkdir()
        self.config = self.state / "openclaw.json"
        self.plugin = self.state / "extensions/memory-anolisa"
        self.plugin.mkdir(parents=True)
        (self.plugin / "payload").write_text("installed plugin")
        self.legacy_plugin = self.state / "plugins/memory-anolisa"
        self.legacy_plugin.mkdir(parents=True)
        self.log = self.root / "calls.json"
        self.cli = self.root / "openclaw"
        self.cli.write_text("#!" + sys.executable + "\n" + r"""import json, os, pathlib, shutil, sys
assert "OPENCLAW_HOME" not in os.environ
assert sys.argv[1:] == ["plugins", "uninstall", "memory-anolisa", "--force"]
root = pathlib.Path(os.environ["TEST_ROOT"]).resolve()
state = pathlib.Path(os.environ["OPENCLAW_STATE_DIR"]).resolve()
assert state.is_relative_to(root)
pathlib.Path(os.environ["TEST_LOG"]).write_text(json.dumps(sys.argv[1:]))
if os.environ.get("TEST_FAIL"):
    print("synthetic host failure", file=sys.stderr)
    sys.exit(19)
# The host, not the wrapper, chooses the write target and removes its own record.
config = pathlib.Path(os.environ["TEST_HOST_CONFIG"]).resolve()
assert config.is_relative_to(root)
data = json.loads(config.read_text())
p = data["plugins"] if "plugins" in data else data
p["allow"] = [x for x in p.get("allow", []) if x != "memory-anolisa"]
p.get("entries", {}).pop("memory-anolisa", None)
config.write_text(json.dumps(data, indent=2) + "\n")
shutil.rmtree(state / "extensions/memory-anolisa")
print("host uninstall completed")
""")
        self.cli.chmod(0o755)
        self.env = {
            "PATH": "/usr/bin:/bin",
            "HOME": str(self.home),
            "OPENCLAW_HOME": str(self.home / "unused"),
            "OPENCLAW_STATE_DIR": str(self.state),
            "OPENCLAW_BIN": str(self.cli),
            "TEST_ROOT": str(self.root),
            "TEST_LOG": str(self.log),
            "TEST_HOST_CONFIG": str(self.config),
        }
        self.fields = {
            "allow": ["memory-anolisa", "other-plugin"],
            "entries": {"memory-anolisa": {"enabled": True}, "other-plugin": {"enabled": True}},
            "slots": {"memory": "other-plugin"},
        }
        self.config.write_text(json.dumps({"plugins": self.fields}))

    def run_script(self, **env: str) -> subprocess.CompletedProcess[str]:
        # All state paths are under this case's exclusive temporary directory.
        for path in (self.state, self.home, self.config, self.plugin):
            self.assertTrue(path.resolve().is_relative_to(self.root))
        return subprocess.run(
            ["/bin/bash", str(SCRIPT)],
            env=self.env | env,
            cwd=self.root,
            text=True,
            capture_output=True,
            timeout=10,
        )

    def test_success_keeps_include_root_byte_exact(self) -> None:
        included = self.state / "plugins.json"
        included.write_text(json.dumps(self.fields))
        original = '{"plugins":{"$include":"./plugins.json"}}\n'
        self.config.write_text(original)
        result = self.run_script(TEST_HOST_CONFIG=str(included))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.config.read_text(), original)
        self.assertNotIn("memory-anolisa", json.loads(included.read_text())["entries"])
        self.assertTrue(json.loads(included.read_text())["entries"]["other-plugin"]["enabled"])
        self.assertFalse(self.plugin.exists())
        self.assertEqual(
            json.loads(self.log.read_text()), ["plugins", "uninstall", "memory-anolisa", "--force"]
        )

    def test_success_honors_custom_config_without_touching_default(self) -> None:
        selected = self.state / "custom.json"
        selected.write_text(json.dumps({"plugins": self.fields}))
        original = self.config.read_bytes()
        result = self.run_script(OPENCLAW_CONFIG_PATH=str(selected), TEST_HOST_CONFIG=str(selected))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.config.read_bytes(), original)
        self.assertNotIn("memory-anolisa", json.loads(selected.read_text())["plugins"]["entries"])

    def test_host_failure_is_reported_without_extra_cleanup(self) -> None:
        original = self.config.read_bytes()
        result = self.run_script(TEST_FAIL="1")
        self.assertEqual(result.returncode, 19, result.stdout + result.stderr)
        self.assertEqual(self.config.read_bytes(), original)
        self.assertTrue(self.plugin.is_dir())
        self.assertTrue(self.legacy_plugin.is_dir())
        self.assertNotIn("plugin removed.", result.stdout)

    def assert_missing_cli_preserves_installation(self, config: str) -> None:
        self.config.write_text(config)
        result = self.run_script(OPENCLAW_BIN=str(self.root / "missing-openclaw"))
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.config.read_text(), config)
        self.assertTrue(self.plugin.is_dir())
        self.assertTrue(self.legacy_plugin.is_dir())
        self.assertFalse(self.log.exists())
        self.assertNotIn("plugin removed.", result.stdout)

    def test_missing_cli_preserves_json_installation(self) -> None:
        self.assert_missing_cli_preserves_installation(json.dumps({"plugins": self.fields}))

    def test_missing_cli_preserves_json5_installation(self) -> None:
        self.assert_missing_cli_preserves_installation(
            "// valid JSON5\n{plugins: " + json.dumps(self.fields) + ",}\n"
        )

    def test_dry_run_does_not_call_cli_or_change_config(self) -> None:
        original = self.config.read_bytes()
        result = self.run_script(ANOLISA_DRY_RUN="1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("plugins uninstall memory-anolisa --force", result.stdout)
        self.assertFalse(self.log.exists())
        self.assertEqual(self.config.read_bytes(), original)
        self.assertTrue(self.plugin.is_dir())

    def test_missing_cli_dry_run_previews_refusal(self) -> None:
        original = self.config.read_bytes()
        result = self.run_script(ANOLISA_DRY_RUN="1", OPENCLAW_BIN=str(self.root / "missing"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("would fail", result.stdout)
        self.assertNotIn("rm -rf", result.stdout)
        self.assertEqual(self.config.read_bytes(), original)
        self.assertTrue(self.plugin.is_dir())


if __name__ == "__main__":
    unittest.main()
