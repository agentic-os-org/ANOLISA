"""Prepare ordinary resident policy before a native QwenPaw App accepts work."""

import json
import os
import signal
from pathlib import Path
import stat
import subprocess
import sys
from functools import wraps
from typing import Any


def _private_json(path: str) -> Any:
    if not isinstance(path, str) or not Path(path).is_absolute():
        raise ValueError("AW startup requires an absolute private descriptor")
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as handle:
        metadata = os.fstat(handle.fileno())
        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid()
                or stat.S_IMODE(metadata.st_mode) != 0o600 or metadata.st_size > 65536):
            raise ValueError("AW startup descriptor must be a private bounded regular file")
        data = handle.read(65537)
        if len(data) > 65536:
            raise ValueError("AW startup descriptor exceeds its limit")
    return json.loads(data)


def _prepare(descriptor: str) -> tuple[dict[str, Any], dict[str, Any]]:
    options = _private_json(descriptor)
    for key in ("binary", "config"):
        if not isinstance(options.get(key), str) or not Path(options[key]).is_absolute():
            raise ValueError("AW startup requires absolute binary and configuration paths")
    if not isinstance(options.get("agent"), str) or not 0 < len(options["agent"]) <= 512:
        raise ValueError("AW startup requires a bounded configured target")
    result = subprocess.run([
        options["binary"], "instance", "prepare", "--config", options["config"],
        "--agent", options["agent"],
    ], stdin=subprocess.DEVNULL, capture_output=True, timeout=90, check=False)
    if result.returncode != 0 or len(result.stdout) > 1048576:
        raise RuntimeError("AW native QwenPaw preparation failed before startup")
    return options, json.loads(result.stdout)


def _wire(reply: dict[str, Any]) -> None:
    environment = reply.get("environment", {})
    config = _private_json(environment.get("AW_NATIVE_CONFIG"))
    if config.get("resident") is not True or config.get("resident_policy_reload") is not True:
        raise ValueError("AW QwenPaw startup requires ordinary resident policy")
    plugin = Path(environment["AW_QWENPAW_PLUGIN"])
    if (not plugin.is_absolute() or plugin.is_symlink() or not plugin.is_dir()
            or plugin.name != config.get("plugin_id")):
        raise ValueError("AW native preparation returned an invalid plugin directory")
    metadata = plugin.stat()
    if metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) != 0o700:
        raise ValueError("AW QwenPaw plugin must be a private owned directory")
    os.environ.update(environment)
    os.environ.pop("AW_INSTANCE_BOOTSTRAP", None)

    from qwenpaw.plugins.loader import PluginLoader
    from qwenpaw.runtime.builder import AgentBuilder

    initialize = PluginLoader.__init__
    build = AgentBuilder.build

    @wraps(initialize)
    def with_plugin(self: Any, plugin_dirs: list[Path]) -> None:
        initialize(self, [*plugin_dirs, plugin.parent])

    @wraps(build)
    async def after_ready(self: Any, *args: Any, **kwargs: Any) -> Any:
        # App's HTTP listener can open before asynchronous plugin startup completes.
        receipt = _private_json(config["ready_path"])
        if (receipt.get("adapter") != "qwenpaw" or receipt.get("pid") != os.getpid()
                or receipt.get("token") != config["ready_token"]):
            raise RuntimeError("AW QwenPaw plugin has not confirmed native readiness")
        return await build(self, *args, **kwargs)

    PluginLoader.__init__ = with_plugin
    AgentBuilder.build = after_ready


def install() -> None:
    """Keep the native console entrypoint and install its preparation boundary once."""
    if Path(sys.argv[0]).name != "qwenpaw" or sys.argv[1:2] != ["app"]:
        return
    try:
        from qwenpaw.cli import main as native
        original = native.cli
    except Exception as error:
        # Python suppresses ordinary .pth errors and would otherwise launch unguarded.
        raise SystemExit("AW QwenPaw startup could not load its native entrypoint") from error
    if getattr(original, "_aw_native_startup", False):
        return

    def dispatch(*args: Any, **kwargs: Any) -> Any:
        native.cli = original
        options, reply = _prepare(os.environ.get("AW_INSTANCE_BOOTSTRAP"))
        try:
            command = reply.get("command", {})
            if (command.get("args") != sys.argv[1:]
                    or Path(command.get("program", "")).resolve() != Path(sys.argv[0]).resolve()):
                raise RuntimeError("QwenPaw startup command differs from its admitted target")
            _wire(reply)
            # Uvicorn re-raises SIGTERM after graceful shutdown. Unwind the
            # dispatcher so its binding and staged assets are released as well.
            def terminate(signum: int, _frame: Any) -> None:
                raise SystemExit(128 + signum)

            previous = signal.signal(signal.SIGTERM, terminate)
            try:
                return original(*args, **kwargs)
            finally:
                signal.signal(signal.SIGTERM, previous)
        finally:
            result = subprocess.run([
                options["binary"], "instance", "release", "--binding", reply["binding"],
            ], stdin=subprocess.DEVNULL, capture_output=True, timeout=15, check=False)
            if result.returncode:
                raise RuntimeError("AW native QwenPaw release failed after shutdown")

    dispatch._aw_native_startup = True
    native.cli = dispatch
