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


LAUNCH_SPEC = importlib.util.spec_from_file_location(
    "aw_qwenpaw_launch",
    ROOT / "crates/aw-adapter/native/qwenpaw/launch.py",
)
LAUNCH = importlib.util.module_from_spec(LAUNCH_SPEC)
LAUNCH_SPEC.loader.exec_module(LAUNCH)


class ManagedTests(unittest.IsolatedAsyncioTestCase):
    def fixture(self, root, failure=False):
        from contextlib import asynccontextmanager

        class Registry:
            def __init__(self):
                self.manifests = {"user": {"id": "user"}}
                self.middleware = ["user"]
                self.startup = []

            def get_plugin_manifest(self, identity):
                return self.manifests.get(identity)

            def register_plugin_manifest(self, identity, manifest):
                self.manifests[identity] = manifest

            def unregister_plugin(self, identity):
                self.manifests.pop(identity, None)
                self.middleware[:] = [x for x in self.middleware if x != identity]
                self.startup[:] = [x for x in self.startup if x != identity]

        class Api:
            def __init__(self, identity, config, manifest):
                self.identity, self.config, self.manifest = identity, config, manifest

            def set_registry(self, registry):
                self.registry = registry

        registry = Registry()

        class Loader:
            def __init__(self):
                self.registry = registry
                self.native_calls = []
                self.lock = asyncio.Lock()

            async def load_all_plugins(self, configs=None, types=None):
                self.native_calls.append((configs, types))
                return {"user": "original-record"}

            @asynccontextmanager
            async def plugin_lifecycle(self, identity):
                async with self.lock:
                    yield

        class Builder:
            calls = 0

            async def build(self):
                self.calls += 1
                return "native-agent"

        modules = {}
        for name, values in {
            "qwenpaw.plugins.loader": {"PluginLoader": Loader},
            "qwenpaw.plugins.api": {"PluginApi": Api},
            "qwenpaw.runtime.builder": {"AgentBuilder": Builder},
        }.items():
            module = types.ModuleType(name)
            module.__dict__.update(values)
            modules[name] = module
        source = root / "aw-owned"
        source.mkdir(mode=0o700)
        config = {
            "resident": True,
            "resident_policy_reload": True,
            "plugin_id": source.name,
            "ready_token": "fixture-token",
            "ready_path": str(root / "ready.json"),
            "hooks": [],
        }
        (source / "plugin.py").write_text("""class Plugin:
    def register(self, api):
        api.registry.middleware.append(api.identity)
        api.registry.startup.append(api.identity)
        if api.config.get('failure'):raise RuntimeError('registration rejected')
plugin = Plugin()
""")
        (source / "aw_native.py").write_text("""from qwenpaw.runtime.builder import AgentBuilder
def install_guard(*, build=None):
    if getattr(AgentBuilder, '_guarded', False):return
    original = AgentBuilder.build if build is None else build
    async def guarded(self, *args, **kwargs):
        return 'guarded:' + await original(self, *args, **kwargs)
    if build is None:AgentBuilder.build = guarded
    AgentBuilder._guarded = True
    return guarded
""")
        manifest = source / "plugin.json"
        manifest.write_text(json.dumps({"id": source.name}))
        manifest.chmod(0o600)
        return source, config, modules, Loader, Builder

    def ready(self, config, **changes):
        value = {
            "version": 1,
            "adapter": "qwenpaw",
            "pid": os.getpid(),
            "token": config["ready_token"],
            "hooks": len(config["hooks"]),
        }
        value.update(changes)
        path = Path(config["ready_path"])
        path.write_text(json.dumps(value))
        path.chmod(0o600)

    async def test_descriptor_limits_preserve_large_native_configuration(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.json"
            value = {"configuration": "x" * 70000}
            path.write_text(json.dumps(value))
            path.chmod(0o600)
            with self.assertRaises(ValueError):
                LAUNCH.private_json(path)
            self.assertEqual(LAUNCH.private_json(path, limit=1024 * 1024), value)
            with path.open("wb") as stream:
                stream.write(b"x" * (1024 * 1024 + 1))
            with self.assertRaises(ValueError):
                LAUNCH.private_json(path, limit=1024 * 1024)

    async def test_runtime_import_waits_for_the_native_plugin_phase(self):
        with tempfile.TemporaryDirectory() as directory:
            source, config, modules, Loader, _ = self.fixture(Path(directory))
            with patch.dict(sys.modules, modules), patch.object(
                LAUNCH, "load_runtime", wraps=LAUNCH.load_runtime
            ) as load:
                LAUNCH.install(source, config)
                load.assert_not_called()
                await Loader().load_all_plugins(types=["channel"])
                load.assert_called_once_with(source, config)

    async def test_native_loads_precede_idempotent_managed_registration(self):
        with tempfile.TemporaryDirectory() as directory:
            source, config, modules, Loader, _ = self.fixture(Path(directory))
            with patch.dict(sys.modules, modules):
                LAUNCH.install(source, config)
                loader = Loader()
                settings = {"user": {"enabled": True}}
                result = await loader.load_all_plugins(configs=settings, types=["channel"])
                self.assertEqual(loader.registry.middleware, ["user"])
                self.assertEqual(result, {"user": "original-record"})
                await asyncio.gather(
                    loader.load_all_plugins(configs=settings),
                    loader.load_all_plugins(configs=settings),
                )
                self.assertEqual(loader.native_calls[0], (settings, ["channel"]))
                self.assertEqual(loader.registry.middleware, ["user", source.name])
                self.assertEqual(loader.registry.startup, [source.name])
                self.assertEqual(loader.registry.manifests["user"], {"id": "user"})
                self.assertNotIn("aw_native", sys.modules)

    async def test_loaders_share_registration_and_preserve_unrelated_entries(self):
        with tempfile.TemporaryDirectory() as directory:
            source, config, modules, Loader, _ = self.fixture(Path(directory))
            with patch.dict(sys.modules, modules):
                LAUNCH.install(source, config)
                first, second = Loader(), Loader()
                self.assertIs(first.registry, second.registry)
                first.registry.startup.append("user")
                await first.load_all_plugins()
                await second.load_all_plugins()
                await asyncio.gather(first.load_all_plugins(), second.load_all_plugins())
                self.assertEqual(first.registry.middleware, ["user", source.name])
                self.assertEqual(first.registry.startup, ["user", source.name])
                first.registry.unregister_plugin(source.name)
                await asyncio.gather(second.load_all_plugins(), first.load_all_plugins())
                self.assertEqual(first.registry.middleware, ["user", source.name])
                self.assertEqual(first.registry.startup, ["user", source.name])

    async def test_failed_registration_can_retry_through_another_loader(self):
        with tempfile.TemporaryDirectory() as directory:
            source, config, modules, Loader, _ = self.fixture(Path(directory))
            with patch.dict(sys.modules, modules):
                LAUNCH.install(source, config)
                first, second = Loader(), Loader()
                with self.assertRaisesRegex(RuntimeError, "registration rejected"):
                    await first.load_all_plugins(configs={source.name: {"failure": True}})
                self.assertEqual(first.registry.manifests, {"user": {"id": "user"}})
                self.assertEqual(first.registry.middleware, ["user"])
                self.assertEqual(first.registry.startup, [])
                await second.load_all_plugins()
                await first.load_all_plugins()
                self.assertEqual(first.registry.middleware, ["user", source.name])
                self.assertEqual(first.registry.startup, [source.name])

    async def test_registration_follows_the_actual_registry_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            source, config, modules, Loader, _ = self.fixture(Path(directory))
            with patch.dict(sys.modules, modules):
                LAUNCH.install(source, config)
                first, second = Loader(), Loader()
                second.registry = type(first.registry)()
                await asyncio.gather(first.load_all_plugins(), second.load_all_plugins())
                for loader in (first, second):
                    self.assertEqual(loader.registry.middleware, ["user", source.name])
                    self.assertEqual(loader.registry.startup, [source.name])

    async def test_early_build_waits_and_enters_the_native_assembly_guard(self):
        with tempfile.TemporaryDirectory() as directory:
            source, config, modules, Loader, Builder = self.fixture(Path(directory))
            with patch.dict(sys.modules, modules):
                LAUNCH.install(source, config)
                builder = Builder()
                task = asyncio.create_task(builder.build())
                await asyncio.sleep(0.01)
                self.assertEqual(builder.calls, 0)
                self.assertFalse(task.done())
                self.ready(config)
                await asyncio.sleep(0.03)
                self.assertEqual(builder.calls, 0)
                self.assertFalse(task.done())
                await Loader().load_all_plugins(types=["channel"])
                await Loader().load_all_plugins()
                self.assertEqual(await task, "guarded:native-agent")
                self.assertEqual(builder.calls, 1)

    async def test_ready_identity_and_permissions_gate_every_build(self):
        with tempfile.TemporaryDirectory() as directory:
            source, config, modules, Loader, Builder = self.fixture(Path(directory))
            with patch.dict(sys.modules, modules):
                LAUNCH.install(source, config)
                await Loader().load_all_plugins(types=["channel"])
                builder = Builder()
                for changed in (
                    {"pid": os.getpid() + 1},
                    {"token": "other"},
                    {"hooks": 1},
                    {"adapter": "other"},
                    {"version": 2},
                ):
                    self.ready(config, **changed)
                    with self.assertRaisesRegex(RuntimeError, "readiness"):
                        await builder.build()
                self.ready(config)
                Path(config["ready_path"]).chmod(0o644)
                with self.assertRaises(ValueError):
                    await builder.build()
                self.assertEqual(builder.calls, 0)

    async def test_missing_ready_has_a_deadline_and_remains_fail_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            source, config, modules, Loader, Builder = self.fixture(Path(directory))
            with patch.dict(sys.modules, modules), patch.object(LAUNCH, "READY_TIMEOUT", 0.001):
                LAUNCH.install(source, config)
                builder = Builder()
                with self.assertRaisesRegex(RuntimeError, "deadline"):
                    await builder.build()
                self.assertEqual(builder.calls, 0)

    async def test_registration_failure_rolls_back_only_owned_registry_entries(self):
        with tempfile.TemporaryDirectory() as directory:
            source, config, modules, Loader, _ = self.fixture(Path(directory))
            with patch.dict(sys.modules, modules):
                LAUNCH.install(source, config)
                loader = Loader()
                with self.assertRaisesRegex(RuntimeError, "registration rejected"):
                    await loader.load_all_plugins(configs={source.name: {"failure": True}})
                self.assertEqual(loader.registry.manifests, {"user": {"id": "user"}})
                self.assertEqual(loader.registry.middleware, ["user"])
                self.assertIsNone(loader.registry.get_plugin_manifest(source.name))

    async def test_bad_module_removes_its_private_namespace(self):
        with tempfile.TemporaryDirectory() as directory:
            source, config, modules, Loader, _ = self.fixture(Path(directory))
            (source / "plugin.py").write_text("raise RuntimeError('bad module')")
            with patch.dict(sys.modules, modules):
                with self.assertRaisesRegex(SystemExit, "runtime initialization") as error:
                    LAUNCH.install(source, config)
                    await Loader().load_all_plugins(types=["channel"])
                self.assertIsInstance(error.exception.__cause__, RuntimeError)
                self.assertEqual(str(error.exception.__cause__), "bad module")
                self.assertNotIn("aw_managed_fixture_token", sys.modules)

    async def test_managed_mode_requires_matching_private_owned_assets(self):
        with tempfile.TemporaryDirectory() as directory:
            source, config, modules, _, _ = self.fixture(Path(directory))
            with patch.dict(sys.modules, modules):
                with self.assertRaisesRegex(ValueError, "resident"):
                    LAUNCH.install(source, {**config, "resident": False})
                source.chmod(0o755)
                with self.assertRaisesRegex(ValueError, "private"):
                    LAUNCH.install(source, config)
                source.chmod(0o700)
                with self.assertRaisesRegex(ValueError, "owned"):
                    LAUNCH.install(source, {**config, "plugin_id": "other"})
                (source / "plugin.json").write_text('{"id":"other"}')
                with self.assertRaisesRegex(ValueError, "manifest"):
                    LAUNCH.install(source, config)

    async def test_bootstrap_runs_original_console_with_original_arguments(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = root / "config.json"
            config.write_text("{}")
            config.chmod(0o600)
            arguments = ["launch.py", "/venv/bin/qwenpaw", "/owned", "app", "--port", "8096"]
            old_path = sys.path.copy()
            try:
                with patch.dict(os.environ, AW_NATIVE_CONFIG=str(config)), patch.object(
                    sys, "argv", arguments
                ), patch.object(LAUNCH, "install") as install, patch.object(
                    LAUNCH.runpy, "run_path"
                ) as run:
                    def check_native_context(*_):
                        self.assertEqual(sys.argv, ["/venv/bin/qwenpaw", "app", "--port", "8096"])
                        self.assertEqual(sys.path[0], "/venv/bin")
                    install.side_effect = check_native_context
                    LAUNCH.main()
                    install.assert_called_once_with(Path("/owned"), {})
                    run.assert_called_once_with("/venv/bin/qwenpaw", run_name="__main__")
                    self.assertEqual(sys.argv, ["/venv/bin/qwenpaw", "app", "--port", "8096"])
                    self.assertEqual(sys.path[0], "/venv/bin")
            finally:
                sys.path[:] = old_path


if __name__ == "__main__":
    unittest.main()
