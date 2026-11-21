"""Opt-in non-invasive Python import-hook instrumentation over LanguageSource.

Activates only when explicitly invoked or when AW_AUTO_INSTRUMENT=1 is set with
a declarative allowlist specification (AW_AUTO_INSTRUMENT_CONFIG or
AW_AUTO_INSTRUMENT_SPEC). Never intercepts modules outside the allowlist.
"""

from __future__ import annotations

import asyncio
import atexit
from copy import deepcopy
from dataclasses import dataclass
import functools
import importlib.abc
import importlib.machinery
import importlib.util
import inspect
import json
import os
from pathlib import Path
import runpy
import sys
import threading
from typing import Any, Callable, Mapping

from aw_sdk._source_helpers import (
    default_extract_input,
    default_extract_text,
    isolate_call_args,
    snapshot_tool_input,
    tool_executed,
    validate_identity,
)
from aw_sdk._source_mutation import (
    DEFAULT_EVENTS,
    SUPPORTED_EVENTS,
    apply_tool_input,
    apply_tool_result,
)
from aw_sdk.sources import DEFAULT_MAX_STREAM_ITEMS, LanguageSource, SourceSession
from aw_sdk.transport import AwError

_TOP_KEYS = frozenset(
    {
        "endpoint",
        "target",
        "availability",
        "session_id",
        "events",
        "replace_input",
        "replace_result",
        "withhold_result",
        "timeout_ms",
        "cwd",
        "targets",
    }
)
_TARGET_KEYS = frozenset(
    {"module", "qualname", "kind", "name", "buffer_stream", "max_stream_items"}
)
_WRAPPED_ATTR = "_aw_auto_wrapped"
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


@dataclass(frozen=True)
class AutoTarget:
    """Validated allowlist entry naming one module attribute or class method."""

    module: str
    qualname: str
    kind: str
    name: str
    buffer_stream: bool = False
    max_stream_items: int = DEFAULT_MAX_STREAM_ITEMS


def _valid_dotted(value: Any, *, allow_dunder: bool = False) -> bool:
    if not isinstance(value, str) or not value or "*" in value:
        return False
    parts = value.split(".")
    for part in parts:
        if not part.isidentifier():
            return False
        if not allow_dunder and part.startswith("__") and part.endswith("__"):
            return False
    return True


def validate_spec(raw: Mapping[str, Any]) -> dict[str, Any]:
    """Validate an explicit allowlist auto-instrumentation spec, rejecting wildcards."""
    if not isinstance(raw, Mapping) or any(k not in _TOP_KEYS for k in raw):
        raise AwError("source_auto_config_invalid")
    endpoint = raw.get("endpoint")
    target = raw.get("target")
    if not isinstance(endpoint, str) or not endpoint.strip():
        raise AwError("source_auto_config_invalid")
    if not isinstance(target, str) or not target.strip():
        raise AwError("source_auto_config_invalid")
    availability = raw.get("availability", "required")
    if availability not in {"required", "native_fallback"}:
        raise AwError("source_auto_config_invalid")
    events = raw.get("events", list(DEFAULT_EVENTS))
    if (
        not isinstance(events, list)
        or not events
        or any(not isinstance(e, str) or e not in SUPPORTED_EVENTS for e in events)
    ):
        raise AwError("source_auto_config_invalid")
    for flag in ("replace_input", "replace_result", "withhold_result"):
        if not isinstance(raw.get(flag, False), bool):
            raise AwError("source_auto_config_invalid")
    timeout_ms = raw.get("timeout_ms", 5000)
    if type(timeout_ms) is not int or not 100 <= timeout_ms <= 60000:
        raise AwError("source_auto_config_invalid")
    session_id = raw.get("session_id")
    if session_id is not None:
        validate_identity(session_id)
    cwd = raw.get("cwd")
    if cwd is not None and (not isinstance(cwd, str) or not cwd.strip()):
        raise AwError("source_auto_config_invalid")
    raw_targets = raw.get("targets")
    if not isinstance(raw_targets, list) or not raw_targets:
        raise AwError("source_auto_config_invalid")
    parsed_targets: list[AutoTarget] = []
    seen: set[tuple[str, str]] = set()
    for entry in raw_targets:
        if not isinstance(entry, Mapping) or any(k not in _TARGET_KEYS for k in entry):
            raise AwError("source_auto_config_invalid")
        mod = entry.get("module")
        qual = entry.get("qualname")
        kind = entry.get("kind", "tool")
        if not _valid_dotted(mod) or not _valid_dotted(qual):
            raise AwError("source_auto_config_invalid")
        if kind not in {"tool", "input"}:
            raise AwError("source_auto_config_invalid")
        key = (mod, qual)
        if key in seen:
            raise AwError("source_auto_config_invalid")
        seen.add(key)
        tool_name = entry.get("name", qual.rsplit(".", 1)[-1])
        validate_identity(tool_name)
        buf = entry.get("buffer_stream", False)
        max_items = entry.get("max_stream_items", DEFAULT_MAX_STREAM_ITEMS)
        if not isinstance(buf, bool) or type(max_items) is not int or max_items < 1:
            raise AwError("source_auto_config_invalid")
        parsed_targets.append(
            AutoTarget(
                module=mod,
                qualname=qual,
                kind=kind,
                name=tool_name,
                buffer_stream=buf,
                max_stream_items=max_items,
            )
        )
    return {
        "endpoint": endpoint,
        "target": target,
        "availability": availability,
        "session_id": session_id,
        "events": list(dict.fromkeys(events)),
        "replace_input": raw.get("replace_input", False),
        "replace_result": raw.get("replace_result", False),
        "withhold_result": raw.get("withhold_result", False),
        "timeout_ms": timeout_ms,
        "cwd": cwd,
        "targets": parsed_targets,
    }


class _SyncBridge:
    """Dedicated background event loop thread for instrumenting synchronous Python callables."""

    def __init__(self, spec: dict[str, Any]) -> None:
        self._spec = spec
        self._lock = threading.Lock()
        self._loop: asyncio.AbstractEventLoop | None = None
        self._thread: threading.Thread | None = None
        self._source: LanguageSource | None = None
        self._session: SourceSession | None = None
        self._closed = False

    def _ensure_loop(self) -> asyncio.AbstractEventLoop:
        with self._lock:
            if self._closed:
                raise AwError("source_closed")
            if self._loop is not None:
                return self._loop
            ready = threading.Event()
            loop_box: list[asyncio.AbstractEventLoop] = []

            def runner() -> None:
                loop = asyncio.new_event_loop()
                asyncio.set_event_loop(loop)
                loop_box.append(loop)
                ready.set()
                loop.run_forever()
                loop.close()

            self._thread = threading.Thread(target=runner, name="aw-auto-bridge", daemon=True)
            self._thread.start()
            ready.wait()
            self._loop = loop_box[0]
            return self._loop

    async def _ensure_session(self) -> SourceSession:
        if self._session is not None:
            return self._session
        source = _create_source(self._spec)
        await source.connect()
        self._source = source
        self._session = source.session(
            session_id=self._spec["session_id"],
            timeout_ms=self._spec["timeout_ms"],
        )
        return self._session

    def run(self, coro_fn: Callable[[SourceSession], Any]) -> Any:
        loop = self._ensure_loop()

        async def execute() -> Any:
            session = await self._ensure_session()
            return await coro_fn(session)

        return asyncio.run_coroutine_threadsafe(execute(), loop).result()

    def close(self) -> None:
        with self._lock:
            self._closed = True
            loop = self._loop
            thread = self._thread
            self._loop = None
            self._thread = None
        if loop is None:
            return

        async def shutdown() -> None:
            source = self._source
            self._session = None
            self._source = None
            if source is not None:
                await source.close()

        try:
            asyncio.run_coroutine_threadsafe(shutdown(), loop).result(timeout=5)
        finally:
            loop.call_soon_threadsafe(loop.stop)
            if thread is not None:
                thread.join(timeout=5)


def _create_source(spec: dict[str, Any]) -> LanguageSource:
    return LanguageSource.create(
        endpoint=spec["endpoint"],
        target=spec["target"],
        availability=spec["availability"],
        events=spec["events"],
        replace_input=spec["replace_input"],
        replace_result=spec["replace_result"],
        withhold_result=spec["withhold_result"],
        cwd=spec["cwd"],
    )


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
                if spec is not None and spec.loader is not None:
                    if hasattr(spec.loader, "exec_module"):
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
        self._patched: list[tuple[Any, str, Any, Any]] = []
        self._sync = _SyncBridge(spec)
        self._async_sessions: dict[int, tuple[LanguageSource, SourceSession]] = {}
        self._lock = threading.Lock()
        self.active = True
        self._finder = _AllowlistFinder(frozenset(self._by_module), self._apply_module)
        sys.meta_path.insert(0, self._finder)
        for mod_name in self._by_module:
            module = sys.modules.get(mod_name)
            if module is not None:
                self._apply_module(module)

    async def _async_session(self) -> SourceSession:
        loop = asyncio.get_running_loop()
        key = id(loop)
        existing = self._async_sessions.get(key)
        if existing is not None:
            return existing[1]
        source = _create_source(self.spec)
        await source.connect()
        session = source.session(
            session_id=self.spec["session_id"],
            timeout_ms=self.spec["timeout_ms"],
        )
        self._async_sessions[key] = (source, session)
        return session

    def _apply_module(self, module: Any) -> None:
        if not self.active:
            return
        targets = self._by_module.get(getattr(module, "__name__", ""))
        if not targets:
            return
        for entry in targets:
            parts = entry.qualname.split(".")
            owner = module
            for segment in parts[:-1]:
                owner = getattr(owner, segment, None)
                if owner is None:
                    break
            if owner is None:
                continue
            attr = parts[-1]
            if not hasattr(owner, attr):
                continue
            is_class = isinstance(owner, type)
            raw_attr = owner.__dict__.get(attr) if is_class else getattr(owner, attr)
            if raw_attr is None:
                continue
            if getattr(raw_attr, _WRAPPED_ATTR, False):
                continue
            if isinstance(raw_attr, (staticmethod, classmethod)):
                continue
            if not callable(raw_attr):
                continue
            wrapped = self._build_wrapper(raw_attr, entry, is_method=is_class)
            setattr(wrapped, _WRAPPED_ATTR, True)
            setattr(owner, attr, wrapped)
            with self._lock:
                self._patched.append((owner, attr, raw_attr, wrapped))

    def _build_wrapper(
        self, original: Callable[..., Any], entry: AutoTarget, *, is_method: bool
    ) -> Callable[..., Any]:
        if inspect.isasyncgenfunction(original):
            return self._wrap_async_gen(original, entry, is_method=is_method)
        if inspect.iscoroutinefunction(original):
            return self._wrap_async(original, entry, is_method=is_method)
        return self._wrap_sync(original, entry, is_method=is_method)

    def _wrap_async(
        self, original: Callable[..., Any], entry: AutoTarget, *, is_method: bool
    ) -> Callable[..., Any]:
        @functools.wraps(original)
        async def wrapper(*args: Any, **kwargs: Any) -> Any:
            if not self.active:
                return await original(*args, **kwargs)
            session = await self._async_session()
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
            session = await self._async_session()
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
                )
            inner = wrapped(*call_args, **kwargs)
            try:
                async for item in inner:
                    yield item
            finally:
                await inner.aclose()

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
            call_args, call_kwargs = isolate_call_args(user_args, kwargs, None)
            if entry.kind == "input":
                text = default_extract_text(*call_args, **call_kwargs)
                self._sync.run(lambda session: session.submit(text))
                if default_extract_text(*call_args, **call_kwargs) != text:
                    raise AwError("source_input_mutated")
                return original(*receiver, *call_args, **call_kwargs)
            raw_input = default_extract_input(*call_args, **call_kwargs)
            orig_snapshot = snapshot_tool_input(raw_input)
            call_id, snapshot, decision = self._sync.run(
                lambda session: session.before_tool(entry.name, raw_input)
            )
            if snapshot_tool_input(default_extract_input(*call_args, **call_kwargs)) != orig_snapshot:
                raise AwError("source_input_mutated")
            if "updated_input" in decision:
                call_args, call_kwargs = apply_tool_input(
                    decision["updated_input"],
                    call_args,
                    call_kwargs,
                    apply_input=None,
                    custom_extract=False,
                    snapshot_arg=None,
                )
                if snapshot_tool_input(default_extract_input(*call_args, **call_kwargs)) != snapshot:
                    raise AwError("source_input_replace_mismatch")
            result = original(*receiver, *call_args, **call_kwargs)
            after = self._sync.run(
                lambda session: session.after_tool(entry.name, snapshot, result, call_id=call_id)
            )
            if "replaced_result" in after:
                with tool_executed():
                    return apply_tool_result(
                        result, after["replaced_result"], apply_result=None, custom_extract=False
                    )
            return result

        return wrapper

    async def aclose(self) -> None:
        """Restore all patched targets and asynchronously close all owned sessions."""
        self._restore_patches()
        sessions = list(self._async_sessions.values())
        self._async_sessions.clear()
        errors: list[BaseException] = []
        for source, _ in sessions:
            try:
                await source.close()
            except BaseException as error:
                errors.append(error)
        try:
            self._sync.close()
        except BaseException as error:
            errors.append(error)
        if errors:
            raise errors[0]

    def _restore_patches(self) -> None:
        self.active = False
        try:
            sys.meta_path.remove(self._finder)
        except ValueError:
            pass
        with self._lock:
            entries = list(reversed(self._patched))
            self._patched.clear()
        for owner, attr, original, wrapped in entries:
            is_class = isinstance(owner, type)
            current = owner.__dict__.get(attr) if is_class else getattr(owner, attr, None)
            if current is wrapped:
                setattr(owner, attr, original)

    def uninstall(self) -> None:
        """Restore patches, remove import hook and release owned AW resources."""
        self._restore_patches()
        self._sync.close()

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


def bootstrap() -> AutoInstrumentation | None:
    """Entrypoint for sitecustomize.py; no-op unless AW_AUTO_INSTRUMENT=1."""
    global _ACTIVE_HANDLE
    if _ACTIVE_HANDLE is not None and _ACTIVE_HANDLE.active:
        return _ACTIVE_HANDLE
    handle = install_from_env()
    if handle is not None:
        _ACTIVE_HANDLE = handle
        atexit.register(handle.uninstall)
    return handle


def _main(argv: list[str]) -> int:
    if not argv:
        raise SystemExit("usage: python3 -m aw_sdk.auto_instrument [-m module | script.py] [args...]")
    handle = install_from_env()
    try:
        if argv[0] == "-m":
            if len(argv) < 2:
                raise SystemExit("missing module name after -m")
            sys.argv = argv[1:]
            runpy.run_module(argv[1], run_name="__main__", alter_sys=True)
        else:
            sys.argv = argv
            runpy.run_path(argv[0], run_name="__main__")
        return 0
    finally:
        if handle is not None:
            handle.uninstall()


if __name__ == "__main__":
    raise SystemExit(_main(sys.argv[1:]))
