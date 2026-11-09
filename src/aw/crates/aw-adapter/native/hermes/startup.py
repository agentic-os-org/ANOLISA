"""Prepare a native Hermes CLI before dispatch in an explicitly configured venv."""

import importlib.abc
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import selectors
import stat
import subprocess
import sys
import time
from typing import Any


def _private_json(path: str) -> Any:
    if not isinstance(path, str) or not Path(path).is_absolute():
        raise ValueError("AW bootstrap requires an absolute private descriptor")
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as handle:
        metadata = os.fstat(handle.fileno())
        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid()
                or stat.S_IMODE(metadata.st_mode) != 0o600 or metadata.st_size > 65536):
            raise ValueError("AW bootstrap descriptor must be a private bounded regular file")
        data = handle.read(65537)
        if len(data) > 65536:
            raise ValueError("AW bootstrap descriptor exceeds its limit")
    return json.loads(data)


def prepare(descriptor: str) -> tuple[dict[str, Any], dict[str, Any], Path]:
    """Resolve the ordinary prepared guard without spawning another Hermes process."""
    options = _private_json(descriptor)
    for key in ("binary", "config", "profile"):
        if not isinstance(options.get(key), str) or not Path(options[key]).is_absolute():
            raise ValueError("AW bootstrap requires binary, config and target")
    if not isinstance(options.get("agent"), str) or not 0 < len(options["agent"]) <= 512:
        raise ValueError("AW bootstrap requires a bounded target")
    # The child is the native owner's direct registration peer.
    argv = [options["binary"], "instance", "prepare", "--config", options["config"],
            "--agent", options["agent"], "--native-profile", options["profile"]]
    output = bytearray()
    deadline = time.monotonic() + 90
    with subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                          stderr=subprocess.DEVNULL) as child:
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(child.stdout, selectors.EVENT_READ)
                while selector.get_map():
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise TimeoutError("AW native preparation exceeded its deadline")
                    for key, _ in selector.select(remaining):
                        data = os.read(key.fd, 65536)
                        if not data:
                            selector.unregister(key.fileobj)
                        output.extend(data)
                        if len(output) > 1024 * 1024:
                            raise ValueError("AW native preparation output exceeds its limit")
            if child.wait(timeout=max(0.001, deadline - time.monotonic())) != 0:
                raise RuntimeError("AW native Hermes preparation failed before startup")
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=5)
    reply = json.loads(output)
    try:
        if not isinstance(reply.get("instance_id"), str) or not reply["instance_id"]:
            raise ValueError("AW native preparation returned an invalid instance")
        launch_path = reply.get("environment", {}).get("AW_HERMES_LAUNCH")
        launch = _private_json(launch_path)
        if launch.get("schema") != "aw-hermes/v1alpha1":
            raise ValueError("AW native preparation returned an invalid Hermes launch")
        guard = Path(launch_path).with_name("hermes-entrypoint.py")
        info = guard.lstat()
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                or stat.S_IMODE(info.st_mode) != 0o600):
            raise ValueError("AW Hermes guard must be a private regular file")
        argv = reply.get("command", {}).get("args")
        if not isinstance(argv, list) or any(not isinstance(value, str) for value in argv):
            raise ValueError("AW native preparation returned invalid arguments")
        positions = [index for index, value in enumerate(argv) if value == str(guard)]
        if len(positions) != 1 or len(argv) <= positions[0] + 1:
            raise ValueError("AW native preparation omitted its guarded command")
        native_args = argv[positions[0] + 1:]
        # Configuration fixes launch options; a later policy update cannot change them.
        supplied = sys.argv[1:]
        configured = native_args[1:]
        if configured[:1] == ["--profile"]:
            configured = configured[2:]
        if [value for value in supplied if value != "--cli"] != [
                value for value in configured if value != "--cli"]:
            raise ValueError("Native Hermes arguments differ from the configured target")
        profile = reply.get("environment", {}).get("HERMES_HOME", options["profile"])
        if profile != options["profile"]:
            raise ValueError("AW native preparation changed the selected profile")
        os.environ["HERMES_HOME"] = profile
        os.environ["AW_HERMES_LAUNCH"] = launch_path
        sys.argv = native_args
        os.environ.pop("AW_INSTANCE_BOOTSTRAP", None)
        return options, reply, guard
    except BaseException as error:
        _release(options, reply, error)
        raise


def _release(options: dict[str, Any], reply: dict[str, Any],
             active: BaseException | None = None) -> None:
    try:
        binding = reply.get("binding")
        if not isinstance(binding, str) or not Path(binding).is_absolute():
            raise ValueError("AW native preparation returned an invalid binding")
        result = subprocess.run([
            options["binary"], "instance", "release", "--binding", binding,
        ], stdin=subprocess.DEVNULL, capture_output=True, timeout=15, check=False)
        if result.returncode:
            raise RuntimeError("AW native Hermes release failed after shutdown")
    except Exception as error:
        if active is None or isinstance(active, SystemExit) and active.code in (None, 0):
            raise
        # Keep the native exit status or traceback while exposing failed cleanup.
        message = f"AW native Hermes cleanup failed: {error}"
        active.add_note(message)
        print(message, file=sys.stderr)


def _wrap(native: Any, prepared: tuple[dict[str, Any], dict[str, Any], Path] | None = None) -> None:
    if getattr(native.main, "_aw_native_startup", False):
        return
    original = native.main

    def dispatch() -> None:
        native.main = original
        options, reply, selected = prepared or prepare(os.environ.get("AW_INSTANCE_BOOTSTRAP"))
        failure = None
        try:
            spec = importlib.util.spec_from_file_location("aw_hermes_guard", selected)
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            sys.argv.insert(0, str(selected))
            sys.path.insert(0, str(selected.parent))
            # Reuse argument integrity, registration, skill and post-tool guards.
            module.main()
        except BaseException as error:
            failure = error
            raise
        finally:
            _release(options, reply, failure)

    dispatch._aw_native_startup = True
    native.main = dispatch


class _Startup(importlib.abc.MetaPathFinder):
    def find_spec(self, fullname: str, path: Any, target: Any = None) -> Any:
        if fullname != "hermes_cli.main":
            return None
        spec = importlib.machinery.PathFinder.find_spec(fullname, path, target)
        if spec is None or spec.loader is None:
            raise ImportError("AW requires the installed Hermes main module")
        original = spec.loader
        startup = self

        class Loader(importlib.abc.Loader):
            def create_module(self, spec: Any) -> Any:
                return original.create_module(spec)

            def exec_module(self, module: Any) -> None:
                prepared = prepare(os.environ.get("AW_INSTANCE_BOOTSTRAP"))
                try:
                    original.exec_module(module)
                    _wrap(module, prepared)
                    sys.meta_path.remove(startup)
                except BaseException as error:
                    _release(prepared[0], prepared[1], error)
                    raise

        spec.loader = Loader()
        return spec


def install() -> None:
    """Install once from a dedicated venv's startup module; management stays native."""
    arguments = sys.argv[1:]
    if (Path(sys.argv[0]).name != "hermes" or arguments[:1] != ["chat"]
            or any(value in ("-h", "--help") for value in arguments[1:])):
        return
    # Python suppresses .pth import errors. Defer all preparation to the real
    # main-module import and dispatch, where failure prevents native execution.
    if "hermes_cli.main" in sys.modules:
        _wrap(sys.modules["hermes_cli.main"])
    elif not any(isinstance(finder, _Startup) for finder in sys.meta_path):
        sys.meta_path.insert(0, _Startup())
