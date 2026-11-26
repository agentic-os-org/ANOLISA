"""Fingerprint and patch Python descriptors, including deferred imports."""

from __future__ import annotations

import hashlib
import importlib.abc
import inspect
import json
import struct
import sys
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from importlib.machinery import ModuleSpec
from types import ClassMethodDescriptorType, CodeType, ModuleType
from typing import Any

from aw_sdk.transport import AwError

ABSENT = object()
WRAPPED = "_aw_source_wrapped"


def _constant(value: Any) -> Any:
    if isinstance(value, CodeType):
        return ["code", _code(value)]
    if type(value) is tuple:
        return ["tuple", [_constant(item) for item in value]]
    if type(value) is frozenset:
        return [
            "frozenset",
            sorted((_constant(item) for item in value), key=json.dumps),
        ]
    if type(value) is bytes:
        return ["bytes", value.hex()]
    if type(value) is int:
        return ["int", hex(value)]
    if type(value) is float:
        return ["float", struct.pack(">d", value).hex()]
    if type(value) is complex:
        return ["complex", struct.pack(">dd", value.real, value.imag).hex()]
    if value is Ellipsis:
        return ["ellipsis"]
    if value is None or type(value) in (bool, str):
        return value
    raise AwError("source_probe_fingerprint_unsupported")


def _code(value: CodeType) -> list[Any]:
    return [
        value.co_argcount,
        value.co_posonlyargcount,
        value.co_kwonlyargcount,
        value.co_flags,
        value.co_code.hex(),
        value.co_exceptiontable.hex(),
        [_constant(item) for item in value.co_consts],
        value.co_names,
        value.co_varnames,
        value.co_cellvars,
        value.co_freevars,
    ]


def function_code_sha256(fn: Callable[..., Any] | staticmethod | classmethod) -> str:
    """Hash an unwrapped Python function using AW's code fingerprint format v1.

    The SHA-256 preimage is compact ASCII JSON containing the format tag and the
    code object's argument counts, flags, bytecode, exception table, constants,
    names, local names, cell variables and free variables, in that order.
    Nested code uses the same representation. Constants with no native JSON
    equivalent use type tags, with IEEE-754 bits for floats and complex values.
    The digest is independent of source availability, filenames and line info.
    Compute it using the deployed interpreter version and optimization level.
    Defaults, globals, closure values and decorator code are outside the digest.
    """
    if isinstance(fn, (staticmethod, classmethod)) or inspect.ismethod(fn):
        fn = fn.__func__
    try:
        original = inspect.unwrap(fn)
    except ValueError as error:
        raise AwError("source_probe_fingerprint_unsupported") from error
    if not inspect.isfunction(original):
        raise AwError("source_probe_fingerprint_unsupported")
    document = ["aw-python-code/v1", _code(original.__code__)]
    return hashlib.sha256(
        json.dumps(document, separators=(",", ":")).encode("ascii")
    ).hexdigest()


def _holder(module: ModuleType, qualname: str) -> tuple[ModuleType | type, str]:
    *path, name = qualname.split(".")
    holder = module
    for segment in path:
        holder = inspect.getattr_static(holder, segment, ABSENT)
        if not isinstance(holder, (ModuleType, type)):
            raise AwError("source_probe_target_unavailable")
    return holder, name


@dataclass
class Seam:
    module: ModuleType
    target: dict[str, Any]
    holder: ModuleType | type
    name: str
    original: Any
    fn: Callable[..., Any]
    receiver: bool
    descriptor: type | None
    signature: inspect.Signature | None

    @classmethod
    def resolve(cls, module: ModuleType, target: dict[str, Any]) -> Seam:
        holder, name = _holder(module, target["qualname"])
        raw = inspect.getattr_static(holder, name, ABSENT)
        descriptor = None
        receiver = isinstance(holder, type)
        if receiver and not any(name in vars(base) for base in holder.__mro__):
            raise AwError("source_probe_target_unavailable")
        if type(raw) is staticmethod:
            fn, descriptor, receiver = raw.__func__, staticmethod, False
        elif type(raw) is classmethod:
            fn, descriptor = raw.__func__, classmethod
        elif isinstance(raw, (staticmethod, classmethod)):
            raise AwError("source_probe_target_descriptor_unsupported")
        elif isinstance(raw, ClassMethodDescriptorType):
            fn, descriptor = raw, classmethod
        else:
            fn = raw
        if not (inspect.isroutine(fn) and callable(fn)) or getattr(fn, WRAPPED, False):
            raise AwError("source_probe_target_unavailable")
        shape = target["call"]
        if (
            (inspect.isgeneratorfunction(fn) and shape != "sync_iter")
            or (inspect.isasyncgenfunction(fn) and shape != "async_iter")
            or (inspect.iscoroutinefunction(fn) and shape != "async")
        ):
            raise AwError("source_probe_target_call_shape_mismatch")
        fingerprint = target.get("code_sha256")
        if fingerprint is not None:
            try:
                matches = function_code_sha256(fn) == fingerprint
            except AwError as error:
                raise AwError("source_probe_fingerprint_mismatch") from error
            if not matches:
                raise AwError("source_probe_fingerprint_mismatch")
        try:
            signature = inspect.signature(fn)
        except (TypeError, ValueError):
            signature = None
        if receiver and "tool.before" in target["hooks"]:
            path = target["fields"]["input"]["from"]
            first = next(iter(signature.parameters), None) if signature else None
            if path.split(".")[0] == "receiver" or (
                first
                and (path == f"kwarg.{first}" or path.startswith(f"kwarg.{first}."))
            ):
                raise AwError("source_probe_receiver_input_unsupported")
        return cls(
            module,
            target,
            holder,
            name,
            vars(holder).get(name, ABSENT),
            fn,
            receiver,
            descriptor,
            signature,
        )


@dataclass
class Patch:
    seam: Seam
    replacement: Any

    @classmethod
    def create(cls, seam: Seam, fn: Callable[..., Any]) -> Patch:
        setattr(fn, WRAPPED, True)
        return cls(seam, seam.descriptor(fn) if seam.descriptor else fn)

    def install(self) -> None:
        setattr(self.seam.holder, self.seam.name, self.replacement)

    def intact(self) -> bool:
        seam = self.seam
        if sys.modules.get(seam.target["module"]) is not seam.module:
            return False
        holder, name = _holder(seam.module, seam.target["qualname"])
        return (
            holder is seam.holder and vars(holder).get(name, ABSENT) is self.replacement
        )

    def restore(self) -> None:
        seam = self.seam
        if vars(seam.holder).get(seam.name, ABSENT) is not self.replacement:
            return  # A later owner has taken over the attribute.
        if seam.original is ABSENT:
            delattr(seam.holder, seam.name)
        else:
            setattr(seam.holder, seam.name, seam.original)


class ProfileFinder(importlib.abc.MetaPathFinder):
    """Delegate regular resolution, then wrap a target module before import returns."""

    def __init__(
        self,
        modules: set[str],
        check: Callable[[], None],
        loaded: Callable[[ModuleType], None],
        failed: Callable[[BaseException], bool],
    ) -> None:
        self.modules, self.check, self.loaded, self.failed = (
            modules,
            check,
            loaded,
            failed,
        )

    def find_spec(
        self,
        fullname: str,
        path: Sequence[str] | None = None,
        target: ModuleType | None = None,
    ) -> ModuleSpec | None:
        if fullname not in self.modules:
            return None
        self.check()
        try:
            if target is not None:
                # Reload can overwrite multiple seams before a replacement is verified.
                raise AwError("source_probe_target_reload_unsupported")
            finders = iter(tuple(sys.meta_path))
            for finder in finders:
                if finder is self:
                    break
            else:
                return None
            for finder in finders:
                spec = finder.find_spec(fullname, path, target)
                if spec is not None:
                    if not hasattr(spec.loader, "exec_module"):
                        raise AwError("source_probe_target_loader_unsupported")
                    spec.loader = _ProfileLoader(self, spec.loader)
                    return spec
            raise AwError("source_probe_target_unavailable")
        except Exception as error:
            if self.failed(error):
                raise
        return None

    def loading(self, module: ModuleType) -> bool:
        spec = getattr(module, "__spec__", None)
        loader = getattr(spec, "loader", None)
        return getattr(spec, "_initializing", False) and (
            isinstance(loader, _ProfileLoader) and loader.finder is self
        )


class _ProfileLoader:
    def __init__(self, finder: ProfileFinder, loader: Any) -> None:
        self.finder, self.loader = finder, loader

    def create_module(self, spec: ModuleSpec) -> ModuleType | None:
        try:
            return self.loader.create_module(spec)
        except BaseException as error:
            self.finder.failed(error)
            raise

    def exec_module(self, module: ModuleType) -> None:
        try:
            self.loader.exec_module(module)
        except BaseException as error:
            self.finder.failed(error)
            raise
        # LazyLoader only arms deferred execution. Resolve now, while import
        # still owns the module lock, if the loader installed lazy attributes.
        try:
            name = module.__name__
            self.finder.loaded(sys.modules.get(name, module))
        except BaseException as error:
            if self.finder.failed(error) or not isinstance(error, Exception):
                raise

    def __getattr__(self, name: str) -> Any:
        return getattr(self.loader, name)
