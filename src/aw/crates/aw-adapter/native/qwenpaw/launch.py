"""Register the owned AW runtime through QwenPaw's native middleware registry."""

import asyncio
from functools import wraps
import importlib.util
import json
import os
from pathlib import Path
import runpy
import stat
import sys

READY_TIMEOUT = 30


def private_json(path, limit=65536):
    if not path.is_absolute():
        raise ValueError("AW runtime descriptor must be absolute")
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as stream:
        metadata = os.fstat(stream.fileno())
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_uid != os.getuid()
            or stat.S_IMODE(metadata.st_mode) != 0o600
            or metadata.st_size > limit
        ):
            raise ValueError("AW runtime descriptor must be private and bounded")
        value = stream.read(limit + 1)
        if len(value) > limit:
            raise ValueError("AW runtime descriptor exceeds its limit")
    return json.loads(value)


def install(source, config):
    if config.get("resident") is not True or config.get("resident_policy_reload") is not True:
        raise ValueError("AW managed startup requires ordinary resident policy")
    if (
        not source.is_absolute()
        or source.is_symlink()
        or not source.is_dir()
        or source.name != config["plugin_id"]
    ):
        raise ValueError("AW runtime requires its owned module directory")
    metadata = source.stat()
    if metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) != 0o700:
        raise ValueError("AW runtime directory must be private")
    manifest = private_json(source / "plugin.json")
    if manifest.get("id") != config["plugin_id"]:
        raise ValueError("AW runtime manifest does not match its binding")
    from qwenpaw.plugins.api import PluginApi
    from qwenpaw.plugins.loader import PluginLoader
    from qwenpaw.runtime.builder import AgentBuilder

    load = PluginLoader.load_all_plugins

    module = None
    guarded_build = None
    native_build = AgentBuilder.build

    @wraps(load)
    async def load_managed(self, configs=None, types=None):
        nonlocal module, guarded_build
        if module is None:
            try:
                module = load_runtime(source, config)
                guarded_build = importlib.import_module(
                    module.__name__ + ".aw_native"
                ).install_guard(build=native_build)
            except Exception as error:
                # App catches plugin failures; an absent admission guard must stop it.
                raise SystemExit("AW managed runtime initialization failed") from error
        result = await load(self, configs=configs, types=types)
        if types is None:
            # Loaders share a Registry but own independent lifecycle locks.
            registry = self.registry
            if not hasattr(registry, "_aw_managed_lock"):
                registry._aw_managed_lock = asyncio.Lock()
            async with self.plugin_lifecycle(config["plugin_id"]), registry._aw_managed_lock:
                if registry.get_plugin_manifest(config["plugin_id"]) is None:
                    api = PluginApi(
                        config["plugin_id"],
                        (configs or {}).get(config["plugin_id"], {}),
                        manifest,
                    )
                    api.set_registry(registry)
                    registry.register_plugin_manifest(config["plugin_id"], manifest)
                    try:
                        module.plugin.register(api)
                    except BaseException:
                        registry.unregister_plugin(config["plugin_id"])
                        raise
        return result

    @wraps(native_build)
    async def protected_build(self, *args, **kwargs):
        # Hub seeding can yield to HTTP requests before the first native plugin phase.
        deadline = asyncio.get_running_loop().time() + READY_TIMEOUT
        while True:
            try:
                if guarded_build is None:
                    raise FileNotFoundError
                receipt = private_json(Path(config["ready_path"]))
            except FileNotFoundError:
                if asyncio.get_running_loop().time() >= deadline:
                    raise RuntimeError("AW runtime startup deadline exceeded") from None
                await asyncio.sleep(0.02)
                continue
            if (
                receipt.get("version") != 1
                or receipt.get("adapter") != "qwenpaw"
                or receipt.get("pid") != os.getpid()
                or receipt.get("token") != config["ready_token"]
                or receipt.get("hooks") != len(config["hooks"])
            ):
                raise RuntimeError("AW runtime readiness does not match this process")
            return await guarded_build(self, *args, **kwargs)

    AgentBuilder.build = protected_build
    PluginLoader.load_all_plugins = load_managed


def load_runtime(source, config):
    namespace = "aw_managed_" + config["ready_token"].replace("-", "_")
    if namespace in sys.modules:
        raise RuntimeError("AW runtime namespace already exists")
    spec = importlib.util.spec_from_file_location(
        namespace,
        source / "plugin.py",
        submodule_search_locations=[str(source)],
    )
    if spec is None or spec.loader is None:
        raise RuntimeError("AW runtime module cannot be loaded")
    module = importlib.util.module_from_spec(spec)
    sys.modules[namespace] = module
    try:
        spec.loader.exec_module(module)
    except BaseException:
        for name in list(sys.modules):
            if name == namespace or name.startswith(namespace + "."):
                sys.modules.pop(name, None)
        raise

    return module


def main():
    program, source, *arguments = sys.argv[1:]
    config = private_json(Path(os.environ["AW_NATIVE_CONFIG"]), limit=1024 * 1024)
    sys.argv = [program, *arguments]
    # Preserve native import precedence before loading any framework modules.
    if not sys.flags.safe_path:
        sys.path[0] = str(Path(program).resolve().parent)
    install(Path(source), config)
    runpy.run_path(program, run_name="__main__")


if __name__ == "__main__":
    main()
