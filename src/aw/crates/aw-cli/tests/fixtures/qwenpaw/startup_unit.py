"""Native startup must fence early App requests and preserve plugin search paths."""

import asyncio
import builtins
import importlib.util
import json
import os
import signal
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[5]
SPEC = importlib.util.spec_from_file_location(
    "aw_qwenpaw_startup", ROOT / "crates/aw-adapter/native/qwenpaw/startup.py",
)
STARTUP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(STARTUP)


class StartupTests(unittest.TestCase):
    def test_plugin_directory_and_ready_receipt_gate_native_build(self):
        class Loader:
            def __init__(self, plugin_dirs):
                self.directories = plugin_dirs

        class Builder:
            calls = 0

            async def build(self):
                self.calls += 1
                return "native-agent"

        modules = {}
        for name, values in {
            "qwenpaw": {}, "qwenpaw.plugins": {}, "qwenpaw.runtime": {},
            "qwenpaw.plugins.loader": {"PluginLoader": Loader},
            "qwenpaw.runtime.builder": {"AgentBuilder": Builder},
        }.items():
            module = types.ModuleType(name)
            module.__dict__.update(values)
            modules[name] = module
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plugin = root / "plugins/aw-native-fixture"
            plugin.mkdir(parents=True, mode=0o700)
            ready = root / "ready.json"
            config = root / "config.json"
            config.write_text(json.dumps({
                "resident": True, "resident_policy_reload": True,
                "plugin_id": plugin.name, "ready_path": str(ready), "ready_token": "owned-token",
            }))
            config.chmod(0o600)
            reply = {"environment": {"AW_NATIVE_CONFIG": str(config), "AW_QWENPAW_PLUGIN": str(plugin)}}
            with patch.dict(sys.modules, modules), patch.dict(os.environ):
                STARTUP._wire(reply)
                self.assertEqual(Loader([root / "user-plugins"]).directories,
                                 [root / "user-plugins", plugin.parent])
                builder = Builder()
                with self.assertRaises(FileNotFoundError):
                    asyncio.run(builder.build())
                self.assertEqual(builder.calls, 0)
                ready.write_text(json.dumps({"adapter": "qwenpaw", "pid": os.getpid(), "token": "other"}))
                ready.chmod(0o600)
                with self.assertRaisesRegex(RuntimeError, "readiness"):
                    asyncio.run(builder.build())
                self.assertEqual(builder.calls, 0)
                ready.write_text(json.dumps({"adapter": "qwenpaw", "pid": os.getpid(), "token": "owned-token"}))
                self.assertEqual(asyncio.run(builder.build()), "native-agent")
                self.assertEqual(builder.calls, 1)
                ready.unlink()
                with self.assertRaises(FileNotFoundError):
                    asyncio.run(builder.build())
                self.assertEqual(builder.calls, 1)

    def test_native_arguments_must_match_the_admitted_target(self):
        native = types.ModuleType("qwenpaw.cli.main")
        native.cli = lambda: self.fail("unadmitted native CLI was called")
        package = types.ModuleType("qwenpaw.cli")
        package.main = native
        reply = {"binding": "/binding", "command": {"program": "/venv/bin/qwenpaw", "args": ["app"]}}
        with patch.dict(sys.modules, {"qwenpaw.cli": package, "qwenpaw.cli.main": native}), \
                patch.object(sys, "argv", ["/venv/bin/qwenpaw", "app", "--workers", "2"]), \
                patch.object(STARTUP, "_prepare", return_value=({"binary": "/aw"}, reply)), \
                patch.object(STARTUP.subprocess, "run", return_value=types.SimpleNamespace(returncode=0)) as release:
            STARTUP.install()
            installed = native.cli
            STARTUP.install()
            self.assertIs(native.cli, installed)
            with self.assertRaisesRegex(RuntimeError, "admitted target"):
                native.cli()
            self.assertEqual(release.call_args.args[0], ["/aw", "instance", "release", "--binding", "/binding"])

    def test_targeted_import_failure_cannot_be_swallowed_by_python_site(self):
        original = builtins.__import__

        def load(name, *args, **kwargs):
            if name == "qwenpaw.cli":
                raise ImportError("native entrypoint unavailable")
            return original(name, *args, **kwargs)

        with patch.object(builtins, "__import__", side_effect=load), \
                patch.object(sys, "argv", ["/venv/bin/qwenpaw", "app"]):
            with self.assertRaisesRegex(SystemExit, "native entrypoint"):
                STARTUP.install()
        with patch.object(builtins, "__import__", side_effect=load), \
                patch.object(sys, "argv", ["/venv/bin/python", "unrelated.py"]):
            STARTUP.install()

    def test_native_sigterm_unwinds_binding_release(self):
        native = types.ModuleType("qwenpaw.cli.main")
        native.cli = lambda: signal.raise_signal(signal.SIGTERM)
        package = types.ModuleType("qwenpaw.cli")
        package.main = native
        reply = {"binding": "/binding", "command": {"program": "/venv/bin/qwenpaw", "args": ["app"]}}
        previous = signal.getsignal(signal.SIGTERM)
        with patch.dict(sys.modules, {"qwenpaw.cli": package, "qwenpaw.cli.main": native}), \
                patch.object(sys, "argv", ["/venv/bin/qwenpaw", "app"]), \
                patch.object(STARTUP, "_prepare", return_value=({"binary": "/aw"}, reply)), \
                patch.object(STARTUP, "_wire"), \
                patch.object(STARTUP.subprocess, "run", return_value=types.SimpleNamespace(returncode=0)) as release:
            STARTUP.install()
            with self.assertRaises(SystemExit) as stopped:
                native.cli()
            self.assertEqual(stopped.exception.code, 128 + signal.SIGTERM)
            self.assertEqual(signal.getsignal(signal.SIGTERM), previous)
            self.assertEqual(release.call_args.args[0], ["/aw", "instance", "release", "--binding", "/binding"])

    def test_descriptor_rejects_public_files_and_symlinks(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "descriptor.json"
            path.write_text("{}")
            path.chmod(0o644)
            with self.assertRaises(ValueError):
                STARTUP._private_json(str(path))
            path.chmod(0o600)
            link = path.with_name("link")
            link.symlink_to(path)
            with self.assertRaises(OSError):
                STARTUP._private_json(str(link))
            self.assertEqual(STARTUP._private_json(str(path)), {})


if __name__ == "__main__":
    unittest.main()
