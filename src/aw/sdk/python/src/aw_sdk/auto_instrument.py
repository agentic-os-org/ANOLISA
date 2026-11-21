"""Opt-in non-invasive Python import-hook instrumentation over LanguageSource.

Activates only when explicitly invoked or when AW_AUTO_INSTRUMENT=1 is set with
a declarative allowlist specification (AW_AUTO_INSTRUMENT_CONFIG or
AW_AUTO_INSTRUMENT_SPEC). Never intercepts modules outside the allowlist.
"""

from __future__ import annotations

import asyncio
import atexit
import functools
import importlib
import importlib.abc
import importlib.machinery
import inspect
import json
import os
from pathlib import Path
import runpy
import sys
import threading
from typing import Any, Callable, Mapping

from aw_sdk._auto_runtime import (
    AsyncSessionPool,
    AutoTarget,
    SyncBridge,
    run_sync_gen,
    run_sync_tool,
    validate_spec,
)
from aw_sdk.transport import AwError

_WRAPPED_ATTR = "_aw_auto_wrapped"
_WRAPPED_HANDLE_ATTR = "_aw_auto_handle"
_MISSING = object()
_ACTIVE_HANDLE: AutoInstrumentation | None = None

__all__ = [
    "AutoInstrumentation",
    "AutoTarget",
    "bootstrap",
    "install",
    "install_from_env",
    "load_spec_from_env",
    "validate_spec",
]


def _unwrap_wrapped_marker(obj: Any) -> tuple[bool, Any]:
    target = obj.__func__ if isinstance(obj, (classmethod, staticmethod)) else obj
    return bool(getattr(target, _WRAPPED_ATTR, False)), getattr(
        target, _WRAPPED_HANDLE_ATTR, None
    )


def _lookup_class_raw(owner: type, attr: str) -> tuple[bool, Any]:
    if attr in owner.__dict__:
        return True, owner.__dict__[attr]
    for base in owner.__mro__[1:]:
        if attr in base.__dict__:
            return False, base.__dict__[attr]
    return False, _MISSING


class _WrappingLoader(importlib.abc.Loader):
    """Delegate module execution, then apply allowlisted wrappers once loaded."""

    def __init__(self, inner: Any, on_loaded: Callable[[Any], None]) -> None:
        self._inner = inner
        self._on_loaded = on_loaded

    def create_module(self, spec: importlib.machinery.ModuleSpec) -> Any:
        create = getattr(self._inner, "create_module", None)
        return create(spec) if create is not None else None

    def exec_module(self, module: Any) -> None:
        self._inner.exec_module(module)
        self._on_loaded(module)

    def __getattr__(self, name: str) -> Any:
        return getattr(self._inner, name)


class _AllowlistFinder(importlib.abc.MetaPathFinder):
    """Intercept only explicitly allowlisted module names on sys.meta_path."""

    def __init__(self, modules: frozenset[str], on_loaded: Callable[[Any], None]) -> None:
        self._modules = modules
        self._on_loaded = on_loaded
        self._in_progress: set[str] = set()

    def find_spec(self, fullname: str, path: Any, target: Any = None) -> Any:
        if fullname not in self._modules or fullname in self._in_progress:
            return None
        self._in_progress.add(fullname)
        try:
            for finder in list(sys.meta_path):
                if finder is self:
                    continue
                find = getattr(finder, "find_spec", None)
                if find is None:
                    continue
                spec = find(fullname, path, target)
                if spec is not None:
                    if spec.loader is None or not hasattr(spec.loader, "exec_module"):
                        raise AwError("source_auto_target_unsupported")
                    spec.loader = _WrappingLoader(spec.loader, self._on_loaded)
                    return spec
            return None
        finally:
            self._in_progress.discard(fullname)


class AutoInstrumentation:
    """Reversible allowlist-based import-hook instrumentation handle."""

    def __init__(self, spec: dict[str, Any]) -> None:
        self.spec = spec
        self._by_module: dict[str, list[AutoTarget]] = {}
        for item in spec["targets"]:
            self._by_module.setdefault(item.module, []).append(item)
        self._patched: list[tuple[Any, str, bool, Any, Any]] = []
        self._installed_targets: set[tuple[str, str]] = set()
        self._sync = SyncBridge(spec)
        self._async_pool = AsyncSessionPool(spec)
        self._lock = threading.Lock()
        self.active = True
        self._finder = _AllowlistFinder(frozenset(self._by_module), self._apply_module)
        try:
            sys.meta_path.insert(0, self._finder)
            for mod_name in self._by_module:
                module = sys.modules.get(mod_name)
                if module is not None:
                    self._apply_module(module)
                elif spec.get("eager_import"):
                    importlib.import_module(mod_name)
        except BaseException:
            self._restore_patches()
            raise

    @property
    def installed_targets(self) -> frozenset[tuple[str, str]]:
        with self._lock:
            return frozenset(self._installed_targets)

    @property
    def pending_targets(self) -> frozenset[tuple[str, str]]:
        all_targets = {(item.module, item.qualname) for item in self.spec["targets"]}
        with self._lock:
            return frozenset(all_targets - self._installed_targets)

    def assert_installed(self) -> None:
        """Raise AwError if any configured target module has not yet been imported/installed."""
        if self.pending_targets:
            raise AwError("source_auto_targets_pending")

    def _apply_module(self, module: Any) -> None:
        if not self.active:
            return
        targets = self._by_module.get(getattr(module, "__name__", ""))
        if not targets:
            return
        local_patched: list[tuple[Any, str, bool, Any, Any]] = []
        local_installed: list[tuple[str, str]] = []
        try:
            for entry in targets:
                self._patch_target(module, entry, local_patched, local_installed)
        except BaseException:
            self._rollback_entries(local_patched)
            raise
        with self._lock:
            self._patched.extend(local_patched)
            self._installed_targets.update(local_installed)

    def _patch_target(
        self,
        module: Any,
        entry: AutoTarget,
        local_patched: list[tuple[Any, str, bool, Any, Any]],
        local_installed: list[tuple[str, str]],
    ) -> None:
        parts = entry.qualname.split(".")
        owner: Any = module
        for segment in parts[:-1]:
            if isinstance(owner, type):
                _, nxt = _lookup_class_raw(owner, segment)
            else:
                nxt = getattr(owner, segment, _MISSING)
            if nxt is _MISSING:
                raise AwError("source_auto_target_missing")
            owner = nxt
        attr = parts[-1]
        is_class = isinstance(owner, type)
        if is_class:
            had_own, raw_attr = _lookup_class_raw(owner, attr)
        else:
            obj_dict = getattr(owner, "__dict__", None)
            had_own = isinstance(obj_dict, dict) and attr in obj_dict
            raw_attr = obj_dict[attr] if had_own else getattr(owner, attr, _MISSING)
        if raw_attr is _MISSING:
            raise AwError("source_auto_target_missing")
        is_wrapped, owner_handle = _unwrap_wrapped_marker(raw_attr)
        if is_wrapped:
            if owner_handle is self:
                local_installed.append((entry.module, entry.qualname))
                return
            raise AwError("source_already_instrumented")
        wrapped = self._wrap_descriptor(raw_attr, entry, is_class=is_class)
        try:
            setattr(owner, attr, wrapped)
        except Exception as error:
            raise AwError("source_auto_target_unsupported") from error
        current = owner.__dict__.get(attr) if is_class else getattr(owner, attr, None)
        if current is not wrapped:
            raise AwError("source_auto_target_unsupported")
        local_patched.append((owner, attr, had_own, raw_attr if had_own else None, wrapped))
        local_installed.append((entry.module, entry.qualname))

    def _wrap_descriptor(self, raw_attr: Any, entry: AutoTarget, *, is_class: bool) -> Any:
        if is_class:
            if isinstance(raw_attr, classmethod):
                fn = raw_attr.__func__
                if not callable(fn):
                    raise AwError("source_auto_target_unsupported")
                wrapped_fn = self._build_wrapper(fn, entry, is_method=True)
                self._tag(wrapped_fn)
                return classmethod(wrapped_fn)
            if isinstance(raw_attr, staticmethod):
                fn = raw_attr.__func__
                if not callable(fn):
                    raise AwError("source_auto_target_unsupported")
                wrapped_fn = self._build_wrapper(fn, entry, is_method=False)
                self._tag(wrapped_fn)
                return staticmethod(wrapped_fn)
            if (
                isinstance(raw_attr, property)
                or hasattr(raw_attr, "__set__")
                or hasattr(raw_attr, "__delete__")
                or not callable(raw_attr)
            ):
                raise AwError("source_auto_target_unsupported")
            wrapped_fn = self._build_wrapper(raw_attr, entry, is_method=True)
            self._tag(wrapped_fn)
            return wrapped_fn
        if not callable(raw_attr):
            raise AwError("source_auto_target_unsupported")
        wrapped_fn = self._build_wrapper(raw_attr, entry, is_method=False)
        self._tag(wrapped_fn)
        return wrapped_fn

    def _tag(self, fn: Any) -> None:
        setattr(fn, _WRAPPED_ATTR, True)
        setattr(fn, _WRAPPED_HANDLE_ATTR, self)

    def _build_wrapper(
        self, original: Callable[..., Any], entry: AutoTarget, *, is_method: bool
    ) -> Callable[..., Any]:
        if inspect.isasyncgenfunction(original):
            return self._wrap_async_gen(original, entry, is_method=is_method)
        if inspect.iscoroutinefunction(original):
            return self._wrap_async(original, entry, is_method=is_method)
        if inspect.isgeneratorfunction(original):
            return self._wrap_sync_gen(original, entry, is_method=is_method)
        return self._wrap_sync(original, entry, is_method=is_method)

    def _wrap_async(
        self, original: Callable[..., Any], entry: AutoTarget, *, is_method: bool
    ) -> Callable[..., Any]:
        @functools.wraps(original)
        async def wrapper(*args: Any, **kwargs: Any) -> Any:
            if not self.active:
                return await original(*args, **kwargs)
            session = await self._async_pool.get_session()
            call_fn = (
                functools.partial(original, args[0]) if is_method and args else original
            )
            call_args = args[1:] if is_method and args else args
            if entry.kind == "input":
                wrapped = session.wrap_input(call_fn)
            else:
                wrapped = session.wrap_tool(
                    call_fn,
                    name=entry.name,
                    buffer_stream=entry.buffer_stream,
                    max_stream_items=entry.max_stream_items,
                    max_stream_bytes=entry.max_stream_bytes,
                )
            return await wrapped(*call_args, **kwargs)

        return wrapper

    def _wrap_async_gen(
        self, original: Callable[..., Any], entry: AutoTarget, *, is_method: bool
    ) -> Callable[..., Any]:
        @functools.wraps(original)
        async def wrapper(*args: Any, **kwargs: Any) -> Any:
            if not self.active:
                async for item in original(*args, **kwargs):
                    yield item
                return
            session = await self._async_pool.get_session()
            call_fn = original if not (is_method and args) else _bind_async_gen(original, args[0])
            call_args = args[1:] if is_method and args else args
            if entry.kind == "input":
                wrapped = session.wrap_input(call_fn)
            else:
                wrapped = session.wrap_tool(
                    call_fn,
                    name=entry.name,
                    buffer_stream=entry.buffer_stream,
                    max_stream_items=entry.max_stream_items,
                    max_stream_bytes=entry.max_stream_bytes,
                )
            inner = wrapped(*call_args, **kwargs)
            try:
                async for item in inner:
                    yield item
            finally:
                await inner.aclose()

        return wrapper

    def _wrap_sync_gen(
        self, original: Callable[..., Any], entry: AutoTarget, *, is_method: bool
    ) -> Callable[..., Any]:
        @functools.wraps(original)
        def wrapper(*args: Any, **kwargs: Any) -> Any:
            if not self.active:
                yield from original(*args, **kwargs)
                return
            receiver = args[:1] if is_method and args else ()
            user_args = args[1:] if is_method and args else args
            yield from run_sync_gen(self._sync, original, entry, receiver, user_args, kwargs)

        return wrapper

    def _wrap_sync(
        self, original: Callable[..., Any], entry: AutoTarget, *, is_method: bool
    ) -> Callable[..., Any]:
        @functools.wraps(original)
        def wrapper(*args: Any, **kwargs: Any) -> Any:
            if not self.active:
                return original(*args, **kwargs)
            receiver = args[:1] if is_method and args else ()
            user_args = args[1:] if is_method and args else args
            return run_sync_tool(self._sync, original, entry, receiver, user_args, kwargs)

        return wrapper

    def _rollback_entries(self, entries: list[tuple[Any, str, bool, Any, Any]]) -> None:
        for owner, attr, had_own, original, wrapped in reversed(entries):
            is_class = isinstance(owner, type)
            current = owner.__dict__.get(attr) if is_class else getattr(owner, attr, None)
            if current is wrapped:
                if had_own:
                    setattr(owner, attr, original)
                else:
                    delattr(owner, attr)

    def _restore_patches(self) -> None:
        self.active = False
        try:
            sys.meta_path.remove(self._finder)
        except ValueError:
            pass
        with self._lock:
            entries = list(self._patched)
            self._patched.clear()
            self._installed_targets.clear()
        self._rollback_entries(entries)

    async def aclose(self) -> None:
        """Restore all patched targets and close all owned loop/sync resources cleanly."""
        self._restore_patches()
        errors: list[BaseException] = []
        try:
            await self._async_pool.aclose()
        except BaseException as error:
            errors.append(error)
        try:
            await asyncio.to_thread(self._sync.close)
        except BaseException as error:
            errors.append(error)
        if errors:
            raise errors[0]

    def uninstall(self) -> None:
        """Restore patches, remove import hook and release all owned AW resources."""
        self._restore_patches()
        errors: list[BaseException] = []
        try:
            self._async_pool.close_sync()
        except BaseException as error:
            errors.append(error)
        try:
            self._sync.close()
        except BaseException as error:
            errors.append(error)
        if errors:
            raise errors[0]

    def __enter__(self) -> AutoInstrumentation:
        return self

    def __exit__(self, *_: Any) -> None:
        self.uninstall()

    async def __aenter__(self) -> AutoInstrumentation:
        return self

    async def __aexit__(self, *_: Any) -> None:
        await self.aclose()


def _bind_async_gen(fn: Callable[..., Any], self_obj: Any) -> Callable[..., Any]:
    async def bound(*args: Any, **kwargs: Any) -> Any:
        async for item in fn(self_obj, *args, **kwargs):
            yield item

    return bound


def load_spec_from_env(env: Mapping[str, str] | None = None) -> dict[str, Any] | None:
    """Load and validate an auto-instrumentation spec when AW_AUTO_INSTRUMENT=1."""
    source_env = os.environ if env is None else env
    if source_env.get("AW_AUTO_INSTRUMENT") != "1":
        return None
    raw_json = source_env.get("AW_AUTO_INSTRUMENT_SPEC")
    config_path = source_env.get("AW_AUTO_INSTRUMENT_CONFIG")
    if bool(raw_json) == bool(config_path):
        raise AwError("source_auto_config_invalid")
    try:
        loaded = json.loads(raw_json) if raw_json else json.loads(Path(config_path).read_text("utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise AwError("source_auto_config_invalid") from error
    return validate_spec(loaded)


def install(spec: Mapping[str, Any]) -> AutoInstrumentation:
    """Install allowlist-driven non-invasive instrumentation from a spec mapping."""
    return AutoInstrumentation(validate_spec(spec))


def install_from_env(env: Mapping[str, str] | None = None) -> AutoInstrumentation | None:
    """Install non-invasive instrumentation if AW_AUTO_INSTRUMENT=1 is set."""
    spec = load_spec_from_env(env)
    return None if spec is None else AutoInstrumentation(spec)


def bootstrap(*, fail_fast: bool | None = None) -> AutoInstrumentation | None:
    """Entrypoint for sitecustomize.py; no-op unless AW_AUTO_INSTRUMENT=1.

    When fail_fast is True (or default for required/invalid specs), raises SystemExit(1)
    on failure so CPython's site.execsitecustomize() cannot swallow startup errors.
    """
    global _ACTIVE_HANDLE
    if os.environ.get("AW_AUTO_INSTRUMENT") != "1":
        return None
    if _ACTIVE_HANDLE is not None and _ACTIVE_HANDLE.active:
        return _ACTIVE_HANDLE
    spec: dict[str, Any] | None = None
    try:
        spec = load_spec_from_env()
        handle = None if spec is None else AutoInstrumentation(spec)
    except Exception as error:
        should_exit = (
            fail_fast
            if fail_fast is not None
            else (spec is None or spec.get("availability", "required") == "required")
        )
        if should_exit:
            sys.stderr.write(f"aw_sdk.auto_instrument: startup failed: {error}\n")
            raise SystemExit(1) from error
        raise
    if handle is not None:
        _ACTIVE_HANDLE = handle
        atexit.register(handle.uninstall)
    return handle


def _main(argv: list[str]) -> int:
    if not argv:
        raise SystemExit(
            "usage: python3 -m aw_sdk.auto_instrument [-m module | script.py] [args...]"
        )
    handle = install_from_env()
    orig_argv = list(sys.argv)
    orig_path = list(sys.path)
    try:
        if argv[0] == "-m":
            if len(argv) < 2:
                raise SystemExit("missing module name after -m")
            sys.argv = list(argv[1:])
            runpy.run_module(argv[1], run_name="__main__", alter_sys=True)
        else:
            script_dir = str(Path(argv[0]).resolve().parent)
            if sys.path:
                sys.path[0] = script_dir
            else:
                sys.path.insert(0, script_dir)
            sys.argv = list(argv)
            runpy.run_path(argv[0], run_name="__main__")
        return 0
    finally:
        sys.path[:] = orig_path
        sys.argv[:] = orig_argv
        if handle is not None:
            handle.uninstall()


if __name__ == "__main__":
    raise SystemExit(_main(sys.argv[1:]))
