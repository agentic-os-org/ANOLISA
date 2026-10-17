#!/usr/bin/env python3
"""Regression tests for the OpenClaw installer ``--config`` override (#6558).

``install_openclaw.py --config <path>`` writes that file, but every OpenClaw
child it launches (``gateway stop``/``install``/``restart``/``status --deep``,
``doctor --fix``, ``plugins install``, the ``agent --message`` write-scope smoke
test, and the tokenless plugin install script) inherited the ambient
environment, so they read the default config or an unrelated inherited
override. The installer could then report a passing smoke check against a
different configuration from the one it had just written.

These tests load the real installer module and capture the environment handed to
each child. Nothing reaches the network, no OpenClaw package is installed and no
gateway is started: most tests replace ``run_command`` and the dependency
precheck, and the last one executes a stub ``openclaw`` from a temporary PATH
that records which config file it actually read.

Baseline on unchanged main: 13 of the 14 tests fail - ``OPENCLAW_CONFIG_PATH``
is absent from every child environment, so the helper assertions, the
per-command-site assertions and the end-to-end flow assertions all fail, and the
executable reproduction shows every child reading the fixture's old default
config (``<home>/.openclaw/openclaw.json``) instead of the selected file. Only
``test_state_paths_follow_the_selected_config`` passes, since the
device-auth/pairing paths already derived from ``--config``.
"""

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src"
    / "os-skills"
    / "ai"
    / "install-openclaw"
    / "scripts"
    / "install_openclaw.py"
)

CONFIG_ENV_VAR = "OPENCLAW_CONFIG_PATH"


def load_installer():
    spec = importlib.util.spec_from_file_location("install_openclaw_under_test", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


installer = load_installer()


def make_args(*extra_argv, **overrides):
    """Parse the real CLI, then apply test-only overrides."""
    argv = [
        "install_openclaw.py",
        "--api-key",
        "test-placeholder-key",
        "--skip-preflight",
        *extra_argv,
    ]
    with patch.object(sys, "argv", argv):
        args = installer.parse_args()
    for name, value in overrides.items():
        setattr(args, name.replace("-", "_"), value)
    return args


class ChildCallRecorder:
    """Stand-in for run_command/subprocess.run that records each child env."""

    def __init__(self, returncode=0, stdout="ok", stderr=""):
        self.calls = []
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

    def __call__(self, cmd, **kwargs):
        self.calls.append((list(cmd), kwargs.get("env")))
        return subprocess.CompletedProcess(
            list(cmd), self.returncode, stdout=self.stdout, stderr=self.stderr
        )

    def env_of(self, *needle):
        """Return the env of the first recorded command containing all needles."""
        for cmd, env in self.calls:
            joined = " ".join(str(part) for part in cmd)
            if all(part in joined for part in needle):
                return env
        raise AssertionError(
            "no child command matched %r; recorded: %r"
            % (needle, [" ".join(map(str, cmd)) for cmd, _ in self.calls])
        )

    def openclaw_children(self):
        return [
            (cmd, env)
            for cmd, env in self.calls
            if cmd and str(cmd[0]).endswith("openclaw")
        ]


OPENCLAW_STUB = r"""#!/usr/bin/env python3
import json
import os
import sys
from pathlib import Path

cfg = os.environ.get("OPENCLAW_CONFIG_PATH") or str(
    Path.home() / ".openclaw" / "openclaw.json"
)
primary = None
try:
    data = json.loads(Path(cfg).read_text(encoding="utf-8"))
except (OSError, ValueError):
    data = None
if isinstance(data, dict):
    agents = data.get("agents") or {}
    defaults = agents.get("defaults") or {}
    model = defaults.get("model") or {}
    primary = model.get("primary")

log = os.environ.get("OPENCLAW_STUB_LOG")
if log:
    with open(log, "a", encoding="utf-8") as fh:
        fh.write(json.dumps({"argv": sys.argv[1:], "config": cfg, "primary": primary}) + "\n")
print("ok")
"""


def primary_model(path):
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    return data["agents"]["defaults"]["model"]["primary"]


class ConfigEnvTestCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.tmpdir = Path(self.tmp.name)
        self.selected = self.tmpdir / "selected" / "openclaw.json"

    def test_env_stamps_the_selected_config_path(self):
        args = make_args("--config", str(self.selected))
        env = installer.openclaw_env(args)
        self.assertEqual(env[CONFIG_ENV_VAR], str(self.selected))

    def test_env_expands_a_home_relative_config_path(self):
        home = self.tmpdir / "home"
        home.mkdir()
        args = make_args("--config", "~/.openclaw/openclaw.json")
        with patch.dict(os.environ, {"HOME": str(home)}):
            env = installer.openclaw_env(args)
        self.assertEqual(env[CONFIG_ENV_VAR], str(home / ".openclaw" / "openclaw.json"))

    def test_env_keeps_the_symlink_spelling_of_the_config_path(self):
        real = self.tmpdir / "real-openclaw.json"
        real.write_text("{}", encoding="utf-8")
        link = self.tmpdir / "linked-openclaw.json"
        link.symlink_to(real)

        args = make_args("--config", str(link))
        self.assertEqual(installer.openclaw_env(args)[CONFIG_ENV_VAR], str(link))
        self.assertEqual(str(installer.resolved_config_path(args)), str(link))

    def test_env_replaces_an_inherited_override_without_touching_os_environ(self):
        stale = self.tmpdir / "stale-openclaw.json"
        args = make_args("--config", str(self.selected))
        with patch.dict(os.environ, {CONFIG_ENV_VAR: str(stale)}):
            env = installer.openclaw_env(args)
            self.assertEqual(env[CONFIG_ENV_VAR], str(self.selected))
            self.assertEqual(os.environ[CONFIG_ENV_VAR], str(stale))

    def test_env_keeps_extra_variables_from_a_base_environment(self):
        args = make_args("--config", str(self.selected))
        base = {"NPM_CONFIG_REGISTRY": "https://registry.example.invalid", "PATH": "/bin"}
        env = installer.openclaw_env(args, base=base)
        self.assertEqual(env[CONFIG_ENV_VAR], str(self.selected))
        self.assertEqual(
            env["NPM_CONFIG_REGISTRY"], "https://registry.example.invalid"
        )
        self.assertEqual(env["PATH"], "/bin")

    def test_start_gateway_children_receive_the_selected_config(self):
        args = make_args("--config", str(self.selected), doctor_fix=True)
        recorder = ChildCallRecorder()
        with patch.object(installer, "run_command", recorder), patch.object(
            installer, "clear_openclaw_gateway_port", lambda *_: None
        ), patch.object(installer, "wait_gateway_ready", lambda *_: None), patch.object(
            installer, "ensure_gateway_write_scope", lambda *_: None
        ):
            installer.start_gateway(args)

        commands = [" ".join(map(str, cmd)) for cmd, _ in recorder.calls]
        self.assertEqual(len(commands), 4, commands)
        for _, env in recorder.calls:
            self.assertEqual(env[CONFIG_ENV_VAR], str(self.selected))
        for needle in (
            "doctor --fix",
            "gateway stop",
            "gateway install",
            "gateway restart",
        ):
            self.assertIn(needle, " | ".join(commands))

    def test_write_scope_smoke_test_receives_the_selected_config(self):
        args = make_args("--config", str(self.selected))
        recorder = ChildCallRecorder()
        with patch.object(installer, "run_command", recorder):
            result = installer.run_gateway_write_smoke(args, timeout=5)
        self.assertEqual(result.returncode, 0)
        env = recorder.env_of("openclaw", "agent", "--message")
        self.assertEqual(env[CONFIG_ENV_VAR], str(self.selected))

    def test_deep_gateway_status_receives_the_selected_config(self):
        args = make_args("--config", str(self.selected), gateway_ready_timeout=0)
        recorder = ChildCallRecorder()
        with patch.object(installer, "run_command", recorder), patch.object(
            installer, "print_gateway_logs", lambda *_: None
        ), patch.object(
            installer.socket, "create_connection", side_effect=OSError("refused")
        ):
            installer.wait_gateway_ready(args)
        env = recorder.env_of("openclaw", "gateway", "status", "--deep")
        self.assertEqual(env[CONFIG_ENV_VAR], str(self.selected))

    def test_dingtalk_plugin_install_receives_the_selected_config(self):
        args = make_args("--config", str(self.selected), npm_registry="https://registry.example.invalid")
        recorder = ChildCallRecorder()
        with patch.object(installer, "run_command", recorder):
            installer.install_dingtalk_plugin(args)
        env = recorder.env_of("openclaw", "plugins", "install")
        self.assertEqual(env[CONFIG_ENV_VAR], str(self.selected))
        self.assertEqual(env["NPM_CONFIG_REGISTRY"], "https://registry.example.invalid")

    def test_tokenless_plugin_install_receives_the_selected_config(self):
        adapter = self.tmpdir / "adapter"
        (adapter / "openclaw" / "scripts").mkdir(parents=True)
        (adapter / "manifest.json").write_text("{}", encoding="utf-8")
        install_script = adapter / "openclaw" / "scripts" / "install.sh"
        install_script.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")

        args = make_args("--config", str(self.selected))
        recorder = ChildCallRecorder()
        with patch.object(installer, "run_command", recorder), patch.object(
            installer, "find_tokenless_adapter_dir", lambda: adapter
        ):
            installer.install_tokenless_plugin(args)

        cmd, env = recorder.calls[0]
        self.assertEqual(cmd[:2], ["bash", str(install_script)])
        self.assertEqual(env[CONFIG_ENV_VAR], str(self.selected))
        self.assertEqual(env["ANOLISA_TARGET"], "openclaw")

    def test_state_paths_follow_the_selected_config(self):
        args = make_args("--config", str(self.selected))
        self.assertEqual(
            installer.device_auth_path(args),
            self.selected.parent / "identity" / "device-auth.json",
        )
        self.assertEqual(
            installer.paired_devices_path(args),
            self.selected.parent / "devices" / "paired.json",
        )

    def run_main(self, *extra_argv):
        recorder = ChildCallRecorder()
        argv = [
            "install_openclaw.py",
            "--api-key",
            "test-placeholder-key",
            "--skip-preflight",
            "--skip-install-openclaw",
            *extra_argv,
        ]
        with patch.object(sys, "argv", argv), patch.object(
            installer, "dependency_precheck", lambda *_: None
        ), patch.object(
            installer, "clear_openclaw_gateway_port", lambda *_: None
        ), patch.object(
            installer, "wait_gateway_ready", lambda *_: None
        ), patch.object(
            subprocess, "run", side_effect=recorder
        ):
            installer.main()
        return recorder

    def test_full_flow_stamps_every_openclaw_child(self):
        """The reproduction from the report: no child may see a missing override."""
        with tempfile.TemporaryDirectory() as tmp:
            selected = Path(tmp) / "selected.json"
            recorder = self.run_main("--config", str(selected))

            children = recorder.openclaw_children()
            self.assertGreaterEqual(len(children), 4)
            for cmd, env in children:
                self.assertIsNotNone(env, "no env passed to %r" % (cmd,))
                self.assertEqual(env.get(CONFIG_ENV_VAR), str(selected), cmd)
            self.assertTrue(selected.is_file())

            commands = " | ".join(" ".join(map(str, cmd)) for cmd, _ in children)
            for needle in ("gateway stop", "gateway install", "gateway restart", "agent --message"):
                self.assertIn(needle, commands)

    def test_full_flow_overrides_an_inherited_config_and_writes_the_same_file(self):
        stale = self.tmpdir / "stale-openclaw.json"
        stale.write_text(json.dumps({"models": {"mode": "merge"}}), encoding="utf-8")
        selected = self.tmpdir / "selected-openclaw.json"

        with patch.dict(os.environ, {CONFIG_ENV_VAR: str(stale)}):
            recorder = self.run_main("--config", str(selected))
            self.assertEqual(os.environ[CONFIG_ENV_VAR], str(stale))

        for cmd, env in recorder.openclaw_children():
            self.assertEqual(env.get(CONFIG_ENV_VAR), str(selected), cmd)
        self.assertEqual(stale.read_text(encoding="utf-8"), json.dumps({"models": {"mode": "merge"}}))
        written = json.loads(selected.read_text(encoding="utf-8"))
        self.assertIn("models", written)

    def test_children_really_read_the_selected_config_not_the_default_fixture(self):
        """Executable reproduction: a stub `openclaw` reports which config it read."""
        home = self.tmpdir / "home"
        (home / ".openclaw").mkdir(parents=True)
        stale_default = home / ".openclaw" / "openclaw.json"
        stale_default.write_text(
            json.dumps(
                {"agents": {"defaults": {"model": {"primary": "fixture-old/old-model"}}}}
            ),
            encoding="utf-8",
        )

        bin_dir = self.tmpdir / "bin"
        bin_dir.mkdir()
        stub = bin_dir / "openclaw"
        stub.write_text(OPENCLAW_STUB, encoding="utf-8")
        stub.chmod(0o755)

        selected = self.tmpdir / "selected-openclaw.json"
        child_log = self.tmpdir / "children.jsonl"

        argv = [
            "install_openclaw.py",
            "--config",
            str(selected),
            "--api-key",
            "test-placeholder-key",
            "--skip-preflight",
            "--skip-install-openclaw",
            "--gateway-ready-timeout",
            "0",
        ]
        with patch.object(sys, "argv", argv), patch.dict(
            os.environ,
            {
                "HOME": str(home),
                "PATH": os.pathsep.join([str(bin_dir), os.environ.get("PATH", "")]),
                "OPENCLAW_STUB_LOG": str(child_log),
            },
        ), patch.object(installer, "dependency_precheck", lambda *_: None), patch.object(
            installer, "clear_openclaw_gateway_port", lambda *_: None
        ), patch.object(installer, "print_gateway_logs", lambda *_: None):
            installer.main()

        records = [
            json.loads(line)
            for line in child_log.read_text(encoding="utf-8").splitlines()
            if line.strip()
        ]
        self.assertGreaterEqual(len(records), 4)
        joined = " | ".join(" ".join(r["argv"]) for r in records)
        for needle in ("gateway stop", "gateway install", "gateway restart", "agent --message"):
            self.assertIn(needle, joined)

        written_primary = primary_model(selected)
        self.assertNotEqual(written_primary, "fixture-old/old-model")
        for record in records:
            self.assertEqual(record["config"], str(selected), record["argv"])
            self.assertEqual(record["primary"], written_primary, record["argv"])
        self.assertEqual(primary_model(stale_default), "fixture-old/old-model")


if __name__ == "__main__":
    unittest.main()
