import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "src/skills/ws-ckpt/scripts/auto-snapshot-aw.py"
ROUTE = SCRIPT.with_name("auto-snapshot-route.sh")
OPENCLAW = """\
#!/bin/sh
[ "$*" = 'plugins list --json' ] || exit 2
[ "$OPENCLAW_STATE_DIR" = "$TEST_STATE_DIR" ] || exit 3
printf '%s\\n' "$TEST_PLUGINS"
exit "${TEST_OPENCLAW_EXIT:-0}"
"""
PROVIDER = """\
import json
import os
from pathlib import Path
import sys

if sys.argv[1] == "bind":
    action = "bind"
    assert len(sys.argv) == 6
    assert sys.argv[2:5] == ["--agent", os.environ["WS_CKPT_AW_TARGET"], "--workspace"]
else:
    assert sys.argv[1] == "policy" and sys.argv[2] in ("status", "enable", "disable")
    action = sys.argv[2]
root = Path(__file__).parent
bound = root / "bound_workspace"
workspace = bound.read_text() if bound.exists() else os.environ.get("TEST_BOUND_WORKSPACE", str(root / "work space"))
log = root / "calls"
with log.open("a") as calls:
    print(action, file=calls)
mode = os.environ.get("TEST_PROVIDER_MODE")
if action == "status":
    if os.environ.get("TEST_UNBOUND") == "1" and not bound.exists():
        sys.exit("not bound")
    if mode == "bad_status":
        print("{}")
        sys.exit(0)
    if mode in ("bad_readback", "bad_write_bad_readback") and (root / "attempted").exists():
        sys.exit("unavailable")
elif action == "bind":
    workspace = sys.argv[5]
    assert Path(workspace).is_absolute()
    bound.write_text(workspace)
    if mode == "bad_sync":
        sys.exit("directory sync failed")
else:
    assert sys.argv[3:] == ["--workspace", workspace]
    (root / "attempted").touch()
    if mode in ("bad_write", "bad_write_bad_readback"):
        sys.exit("write failed")
    (root / "enabled").write_text(action)
    if mode == "bad_sync":
        sys.exit("directory sync failed")
print(json.dumps({
    "workspace": workspace,
    "target": "qoder",
    "enabled": (root / "enabled").exists() and (root / "enabled").read_text() == "enable",
    "last_result": None,
}))
"""


WS_CKPT = """\
import json
import os
from pathlib import Path
import sys

root = Path(__file__).parent
assert sys.argv[1:3] == ["status", "--workspace"] and sys.argv[4:] == ["--format", "json"]
workspace = Path(sys.argv[3]).resolve()
(root / "registration_checked").touch()
ids = {
    (root / "work space").resolve(): "ws-primary",
    (root / "elsewhere").resolve(): "ws-different",
}
if os.environ.get("TEST_UNREGISTERED") == "1" or workspace not in ids:
    sys.exit("\x1b[31mError [WorkspaceNotFound]: workspace not found\x1b[0m")
entry = {"path": str(workspace), "ws_id": ids[workspace]}
if os.environ.get("TEST_INVALID_REGISTRATION") == "1":
    del entry["ws_id"]
print(json.dumps({"workspaces": [entry]}))
"""


class SnapshotControlTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.provider = self.root / "mock provider"
        self.provider.write_text(f"#!{sys.executable}\n{PROVIDER}")
        self.provider.chmod(0o700)
        self.workspace = self.root / "work space"
        self.workspace.mkdir()
        cli = self.root / "ws-ckpt"
        cli.write_text(f"#!{sys.executable}\n{WS_CKPT}")
        cli.chmod(0o700)
        self.env = {
            "PATH": f"{self.root}{os.pathsep}{os.defpath}",
            "WS_CKPT_AW_PROVIDER": str(self.provider),
            "WS_CKPT_AW_TARGET": "qoder",
        }

    def invoke(self, *args, cwd=None):
        return subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            env=self.env, cwd=cwd, capture_output=True, text=True,
        )

    def calls(self):
        path = self.root / "calls"
        return path.read_text().splitlines() if path.exists() else []

    def assert_route(self, expected, *args, env):
        result = subprocess.run(
            ["/bin/sh", str(ROUTE), *args], env=env, capture_output=True, text=True
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), expected)

    def test_aw_on_path_leaves_ordinary_session_in_model_mode(self):
        aw = self.root / "aw"
        aw.write_text("#!/bin/sh\nexit 0\n")
        aw.chmod(0o700)
        ordinary = {"PATH": str(self.root)}
        self.assert_route("model", env=ordinary)
        self.assert_route("aw_setup", "--aw", env=ordinary)

    def test_explicit_aw_selection_is_independent_of_native_profile(self):
        ordinary = {"PATH": str(self.root), "HERMES_HOME": str(self.root)}
        self.assert_route("hermes_plugin_missing", env=ordinary)
        self.assert_route("aw_setup", "--aw", env=ordinary)

    def test_explicit_aw_checks_the_current_binding(self):
        self.assert_route("aw_bound", "--aw", env=self.env)
        for missing in ("WS_CKPT_AW_TARGET", "WS_CKPT_AW_PROVIDER"):
            with self.subTest(missing=missing):
                env = {key: value for key, value in self.env.items() if key != missing}
                self.assert_route("aw_unavailable", "--aw", env=env)
                self.assert_route("aw_unavailable", env=env)

    def test_status_is_read_only(self):
        result = self.invoke("status")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(json.loads(result.stdout)["enabled"])
        self.assertEqual(self.calls(), ["status"])
        self.assertFalse((self.root / "registration_checked").exists())

    def test_bind_works_before_a_workspace_is_selected(self):
        self.env["TEST_UNBOUND"] = "1"
        workspace = str(self.root / "new workspace")
        result = self.invoke("bind", "--workspace", workspace)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["workspace"], workspace)
        self.assertFalse(json.loads(result.stdout)["enabled"])
        self.assertEqual(self.calls(), ["bind", "status"])
        self.assertFalse((self.root / "registration_checked").exists())

    def test_bind_requires_an_explicit_absolute_path(self):
        for args in ([], ["--workspace", "relative"]):
            with self.subTest(args=args):
                result = self.invoke("bind", *args)
                self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.calls(), [])

    def test_bind_reports_effective_workspace_after_sync_failure(self):
        self.env["TEST_PROVIDER_MODE"] = "bad_sync"
        workspace = str(self.root / "new workspace")
        result = self.invoke("bind", "--workspace", workspace)
        self.assertEqual(result.returncode, 0, result.stderr)
        data = json.loads(result.stdout)
        self.assertEqual(data["workspace"], workspace)
        self.assertFalse(data["persistence_confirmed"])
        self.assertEqual(self.calls(), ["bind", "status"])

    def test_writes_are_checked_before_and_after(self):
        for action in ("enable", "disable"):
            with self.subTest(action=action):
                result = self.invoke(action, "--workspace", str(self.root / "work space"))
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads(result.stdout)["enabled"], action == "enable")
        self.assertEqual(self.calls(), ["status", "enable", "status", "status", "disable", "status"])

    def test_wrong_workspace_cannot_write(self):
        other = self.root / "elsewhere"
        other.mkdir()
        result = self.invoke("enable", "--workspace", str(other))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not match", result.stderr)
        self.assertEqual(self.calls(), ["status"])

    def test_aw_switch_works_with_inherited_native_profiles(self):
        self.env["OPENCLAW_BIN"] = str(self.root / "missing-openclaw")
        for variable in ("OPENCLAW_HOME", "HERMES_HOME"):
            with self.subTest(variable=variable):
                self.env[variable] = str(self.root)
                for action in ("enable", "disable"):
                    result = self.invoke(action)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(json.loads(result.stdout)["enabled"], action == "enable")
                del self.env[variable]
        self.assertEqual(self.calls(), ["status", "enable", "status", "status", "disable", "status"] * 2)

    def test_invalid_status_cannot_write(self):
        self.env["TEST_PROVIDER_MODE"] = "bad_status"
        self.assertNotEqual(self.invoke("enable").returncode, 0)
        self.assertEqual(self.calls(), ["status"])

    def test_failed_write_is_not_retried(self):
        self.env["TEST_PROVIDER_MODE"] = "bad_write"
        self.assertNotEqual(self.invoke("enable").returncode, 0)
        self.assertEqual(self.calls(), ["status", "enable", "status"])

    def test_failed_readback_reports_that_write_completed(self):
        self.env["TEST_PROVIDER_MODE"] = "bad_readback"
        result = self.invoke("enable")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("current state is unknown", result.stderr)
        self.assertEqual(self.calls(), ["status", "enable", "status"])

    def test_skill_can_enable_from_inside_registered_workspace(self):
        child = self.workspace / "child"
        child.mkdir()
        alias = self.root / "cwd-alias"
        alias.symlink_to(child, target_is_directory=True)
        result = self.invoke("enable", cwd=alias)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(json.loads(result.stdout)["enabled"])
        self.assertTrue((self.root / "registration_checked").exists())
        self.assertEqual(self.calls(), ["status", "enable", "status"])

    def test_registered_workspace_can_enable_without_a_launch_directory(self):
        result = self.invoke("enable")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.root / "registration_checked").exists())
        self.assertTrue(json.loads(result.stdout)["enabled"])

    def test_registered_workspace_binding_can_use_trailing_slash_or_symlink(self):
        alias = self.root / "alias"
        alias.symlink_to(self.workspace, target_is_directory=True)
        for spelling in (f"{self.workspace}/", str(alias)):
            with self.subTest(spelling=spelling):
                self.env["TEST_BOUND_WORKSPACE"] = spelling
                result = self.invoke("enable")
                self.assertEqual(result.returncode, 0, result.stderr)
                data = json.loads(result.stdout)
                self.assertEqual(data["workspace"], spelling)
                self.assertTrue(data["enabled"])

    def test_explicit_workspace_alias_uses_daemon_identity(self):
        alias = self.root / "alias"
        alias.symlink_to(self.workspace, target_is_directory=True)
        for action, path in (
            ("enable", f"{self.workspace}/"),
            ("status", str(alias)),
            ("disable", str(alias)),
        ):
            with self.subTest(action=action):
                result = self.invoke(action, "--workspace", path)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads(result.stdout)["enabled"], action != "disable")
        self.assertEqual(
            self.calls(), ["status", "enable", "status", "status", "status", "disable", "status"]
        )

    def test_invalid_registration_report_does_not_prompt_init_or_write(self):
        self.env["TEST_INVALID_REGISTRATION"] = "1"
        result = self.invoke("enable")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid workspace status", result.stderr)
        self.assertNotIn("ws-ckpt init", result.stderr)
        self.assertEqual(self.calls(), ["status"])

    def test_unregistered_workspace_stays_disabled(self):
        self.env["TEST_UNREGISTERED"] = "1"
        result = self.invoke("enable")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not registered", result.stderr)
        self.assertEqual(self.calls(), ["status"])

    def test_failed_disable_reports_enabled_after_readback(self):
        (self.root / "enabled").write_text("enable")
        self.env["TEST_PROVIDER_MODE"] = "bad_write"
        result = self.invoke("disable")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("currently enabled", result.stderr)
        self.assertEqual(self.calls(), ["status", "disable", "status"])

    def test_failed_disable_can_be_visible_in_readback(self):
        (self.root / "enabled").write_text("enable")
        self.env["TEST_PROVIDER_MODE"] = "bad_sync"
        result = self.invoke("disable")
        self.assertEqual(result.returncode, 0, result.stderr)
        data = json.loads(result.stdout)
        self.assertFalse(data["enabled"])
        self.assertFalse(data["persistence_confirmed"])
        self.assertEqual(self.calls(), ["status", "disable", "status"])

    def test_failed_write_and_unavailable_readback_reports_unknown(self):
        self.env["TEST_PROVIDER_MODE"] = "bad_write_bad_readback"
        result = self.invoke("disable")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("write reported an error", result.stderr)
        self.assertIn("current state is unknown", result.stderr)
        self.assertEqual(self.calls(), ["status", "disable", "status"])


class OpenClawRouteTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        openclaw = self.root / "openclaw"
        openclaw.write_text(OPENCLAW)
        openclaw.chmod(0o700)
        self.env = {
            "PATH": f"{Path(sys.executable).parent}{os.pathsep}{os.defpath}",
            "OPENCLAW_BIN": str(openclaw),
            "OPENCLAW_STATE_DIR": str(self.root),
            "TEST_STATE_DIR": str(self.root),
            "TEST_PLUGINS": '{"plugins":[{"id":"unrelated"}]}',
        }

    def invoke(self, *args, script=ROUTE):
        return subprocess.run(["sh", str(script), *args], env=self.env, capture_output=True, text=True)

    def assert_route(self, expected, *args, script=ROUTE):
        result = self.invoke(*args, script=script)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), expected)

    def test_lists_installed_plugin_even_if_disabled(self):
        self.env["TEST_PLUGINS"] = '{"plugins":[{"id":"ws-ckpt","status":"disabled"}]}'
        self.assert_route("openclaw_plugin_present")

    def test_missing_plugin(self):
        self.assert_route("openclaw_plugin_missing")

    def test_openclaw_home_alone_selects_custom_plugin_directory(self):
        self.env["OPENCLAW_HOME"] = self.env.pop("OPENCLAW_STATE_DIR")
        self.assert_route("openclaw_plugin_missing")
        (self.root / "extensions/ws-ckpt").mkdir(parents=True)
        self.assert_route("openclaw_plugin_present")

    def test_explicit_openclaw_checks_extension_directory_in_custom_home(self):
        self.env["OPENCLAW_HOME"] = self.env.pop("OPENCLAW_STATE_DIR")
        (self.root / "extensions/ws-ckpt").mkdir(parents=True)
        self.assert_route("openclaw_plugin_present", "--openclaw")

    def test_aw_binding_takes_precedence_over_inherited_openclaw_home(self):
        self.env["OPENCLAW_HOME"] = self.env.pop("OPENCLAW_STATE_DIR")
        self.env.update(WS_CKPT_AW_TARGET="qoder", WS_CKPT_AW_PROVIDER="/bin/sh")
        self.assert_route("aw_bound")
        self.assert_route("openclaw_plugin_missing", "--openclaw")
        del self.env["WS_CKPT_AW_TARGET"]
        self.assert_route("aw_unavailable")

    def test_route_can_infer_state_directory_from_install_location(self):
        script = self.root / ".openclaw/skills/ws-ckpt/scripts" / ROUTE.name
        script.parent.mkdir(parents=True)
        script.write_bytes(ROUTE.read_bytes())
        del self.env["OPENCLAW_STATE_DIR"]
        self.env["TEST_STATE_DIR"] = str(self.root / ".openclaw")
        self.assert_route("openclaw_plugin_missing", script=script)

    def test_failed_or_invalid_probe_does_not_report_missing(self):
        for overrides in ({"TEST_OPENCLAW_EXIT": "1"}, {"TEST_PLUGINS": "{}"}):
            with self.subTest(overrides=overrides):
                saved = self.env
                self.env = dict(saved, **overrides)
                result = self.invoke()
                self.env = saved
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")

    def test_model_override_skips_openclaw_probe(self):
        self.env["TEST_OPENCLAW_EXIT"] = "1"
        self.assert_route("model", "--model")


class HermesRouteTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.profile = self.root / ".hermes"
        self.env = {"HOME": str(self.root), "PATH": str(self.root)}

    def assert_route(self, expected, *args, script=ROUTE):
        result = subprocess.run(
            ["/bin/sh", str(script), *args], env=self.env, capture_output=True, text=True
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), expected)

    def install_link(self):
        source = self.root / "plugin-source"
        source.mkdir()
        (source / "plugin.yaml").write_text("name: ws-ckpt\n")
        plugins = self.profile / "plugins"
        plugins.mkdir(parents=True)
        (plugins / "ws-ckpt").symlink_to(source, target_is_directory=True)

    def test_explicit_hermes_detects_missing_and_installed_plugin(self):
        self.assert_route("hermes_plugin_missing", "--hermes")
        self.install_link()
        self.assert_route("hermes_plugin_present", "--hermes")

    def test_custom_hermes_home_is_detected(self):
        self.profile = self.root / "custom hermes"
        self.env["HERMES_HOME"] = str(self.profile)
        self.assert_route("hermes_plugin_missing")
        self.install_link()
        self.assert_route("hermes_plugin_present")

    def test_installed_skill_path_identifies_hermes(self):
        script = self.profile / "skills/ws-ckpt/scripts" / ROUTE.name
        script.parent.mkdir(parents=True)
        script.write_bytes(ROUTE.read_bytes())
        self.assert_route("hermes_plugin_missing", script=script)

    def test_installed_plugin_alone_does_not_identify_the_agent(self):
        self.install_link()
        self.assert_route("model")

    def test_aw_binding_takes_precedence_over_inherited_hermes_home(self):
        self.install_link()
        self.env.update(
            HERMES_HOME=str(self.profile),
            WS_CKPT_AW_TARGET="qoder",
            WS_CKPT_AW_PROVIDER="/bin/sh",
        )
        self.assert_route("aw_bound")
        self.assert_route("hermes_plugin_present", "--hermes")
        self.assert_route("model", "--model")


if __name__ == "__main__":
    unittest.main()
