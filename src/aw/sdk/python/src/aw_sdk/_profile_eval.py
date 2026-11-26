"""Validate Python instrumentation profiles and evaluate their bounded field paths."""

from __future__ import annotations

import json
import operator
import re
from collections.abc import Callable
from copy import deepcopy
from dataclasses import dataclass
from inspect import Parameter, Signature, getattr_static
from types import MemberDescriptorType
from typing import Any

from aw_sdk._source_helpers import is_plain_json, snapshot_input_arg
from aw_sdk.deployment import object_fields
from aw_sdk.transport import AwError

VERSION = "aw-instrumentation-profile/v1alpha1"
MAX_PROFILE_BYTES = 1024 * 1024
IDENT = r"(?!__)[A-Za-z_][A-Za-z0-9_]*"
COMPARATOR = re.compile(r"(>=|<=|>|<|==)([0-9]+(?:\.[0-9]+){0,3})")
# PEP 440 spellings are accepted, with comparisons defined over release segments.
INSTALLED_VERSION = re.compile(
    r"v?(?:(0*[0-9]+)!)?([0-9]+(?:\.[0-9]+)*)"
    r"(?:[-_.]?(?:alpha|a|beta|b|preview|pre|rc|c)[-_.]?[0-9]*)?"
    r"(?:-[0-9]+|[-_.]?(?:post|rev|r)[-_.]?[0-9]*)?"
    r"(?:[-_.]?dev[-_.]?[0-9]*)?(?:\+[a-z0-9]+(?:[-_.][a-z0-9]+)*)?",
    re.ASCII | re.IGNORECASE,
)
COMPARISONS = {
    ">=": operator.ge,
    "<=": operator.le,
    ">": operator.gt,
    "<": operator.lt,
    "==": operator.eq,
}
EFFECTS = {
    "session.start": (),
    "session.end": (),
    "turn.stop": ("block",),
    "input.submit": ("block",),
    "tool.before": ("block", "replace_input", "ask"),
    "tool.after": ("replace_result",),
    "permission.request": ("block", "ask"),
    "compact.before": ("block",),
    "compact.after": (),
    "subagent.start": ("block",),
    "subagent.stop": (),
    "model.before_request": ("block",),
    "security.violation": ("block",),
}
TOOL_FIELDS = {"tool_name", "call_id", "input"}
EVENT_FIELDS = {
    "tool.before": TOOL_FIELDS,
    "tool.after": TOOL_FIELDS | {"result"},
    "permission.request": TOOL_FIELDS,
    "input.submit": {"prompt"},
    "turn.stop": {"output"},
    "security.violation": {"output"},
    "model.before_request": {"request"},
}
REQUIRED_FIELDS = {
    "tool.before": {"tool_name", "input"},
    "tool.after": {"tool_name", "result"},
    "permission.request": {"tool_name"},
    "input.submit": {"prompt"},
    "security.violation": {"output"},
}
MISSING = object()


def require(condition: bool, detail: str) -> None:
    if not condition:
        error = AwError("source_profile_invalid")
        error.add_note(detail)
        raise error


def _object(value: Any, required: set[str], optional: tuple[str, ...] = ()) -> None:
    require(
        type(value) is dict and required <= value.keys() <= required.union(optional),
        "invalid object fields",
    )


def _matches(value: Any, pattern: str, limit: int) -> bool:
    return (
        isinstance(value, str)
        and len(value) <= limit
        and bool(re.fullmatch(pattern, value))
    )


def _index(value: str, limit: int) -> bool:
    return bool(re.fullmatch(r"0|[1-9][0-9]{0,3}", value)) and int(value) <= limit


def parse_path(value: Any) -> tuple[str, tuple[str, ...]]:
    """Return the root selector and up to six trailing field or index segments."""
    require(isinstance(value, str) and 0 < len(value) <= 256, "invalid field path")
    root, *parts = value.split(".")
    if root in ("arg", "kwarg"):
        require(bool(parts), "missing argument selector")
        selector = parts.pop(0)
        valid = _index(selector, 31) if root == "arg" else re.fullmatch(IDENT, selector)
        require(bool(valid), "invalid argument selector")
        root = f"{root}.{selector}"
    else:
        require(root in ("receiver", "result"), "unsupported field root")
    require(
        len(parts) <= 6
        and all(re.fullmatch(IDENT, p) or _index(p, 1023) for p in parts),
        "invalid field path segment",
    )
    return root, tuple(parts)


def _sequence(value: Any, allowed: set[str], *, empty: bool = False) -> None:
    require(
        type(value) is list
        and (empty or bool(value))
        and all(isinstance(x, str) and x in allowed for x in value)
        and len(set(value)) == len(value),
        "invalid or repeated capability entry",
    )


def _validate_hook(event: str, cap: Any) -> None:
    _object(cap, {"mode", "effects", "failure_actions"})
    mode, effects, actions = cap["mode"], cap["effects"], cap["failure_actions"]
    require(mode in ("observe", "control"), "invalid Hook mode")
    _sequence(effects, {"observe", *EFFECTS[event]})
    require("observe" in effects, "Hook capability must include observe")
    allowed = {"report"}
    if "block" in effects:
        allowed.add("block")
    if event == "tool.after":
        allowed.add("withhold_result")
    _sequence(actions, allowed, empty=True)
    if mode == "observe":
        require(
            effects == ["observe"] and set(actions) <= {"report"},
            "observe mode has control",
        )


def _validate_field(name: str, binding: Any) -> None:
    if type(binding) is dict and "const" in binding:
        _object(binding, {"const"})
        value = binding["const"]
        # Python's isspace includes four extra C0 separators outside Unicode White_Space.
        require(
            name == "tool_name"
            and isinstance(value, str)
            and 0 < len(value) <= 128
            and any(not c.isspace() or c in "\x1c\x1d\x1e\x1f" for c in value)
            and not re.search(r"[\r\n\0\u2028\u2029]", value),
            "invalid const field",
        )
    else:
        _object(binding, {"from", "codec"})
        require(
            binding["codec"] in ("text", "json", "json_string"), "invalid field codec"
        )
        root, _ = parse_path(binding["from"])
        require(root != "result" or name in ("result", "output"), "invalid result root")


def _validate_target(target: Any, custom: bool) -> None:
    _object(
        target,
        {"id", "module", "qualname", "call", "hooks", "fields"},
        ("code_sha256", "stream"),
    )
    require(_matches(target["id"], r"[A-Za-z0-9._-]+", 128), "invalid target id")
    require(
        _matches(target["module"], rf"{IDENT}(?:\.{IDENT})*", 256), "invalid module"
    )
    require(
        _matches(target["qualname"], rf"{IDENT}(?:\.{IDENT}){{0,3}}", 256),
        "invalid qualname",
    )
    shape = target["call"]
    require(shape in ("sync", "async", "sync_iter", "async_iter"), "invalid call shape")
    hooks, fields = target["hooks"], target["fields"]
    require(
        type(hooks) is dict and bool(hooks) and hooks.keys() <= EFFECTS.keys(),
        "invalid Hooks",
    )
    require(type(fields) is dict, "invalid fields")
    allowed = {"session_id", "turn_id"}
    for event, cap in hooks.items():
        _validate_hook(event, cap)
        allowed.update(EVENT_FIELDS.get(event, ()))
        require(
            REQUIRED_FIELDS.get(event, set()) <= fields.keys(),
            f"missing {event} fields",
        )
    require(fields.keys() <= allowed, "field disallowed for declared Hooks")
    for name, binding in fields.items():
        _validate_field(name, binding)

    before, after = hooks.get("tool.before"), hooks.get("tool.after")
    replaces_result = after is not None and "replace_result" in after["effects"]
    if before and "replace_input" in before["effects"]:
        require(
            fields["input"]["codec"] in ("json", "json_string"),
            "invalid replace_input codec",
        )
    if replaces_result:
        require(
            parse_path(fields["result"]["from"])[0] == "result",
            "invalid replace_result root",
        )
    if "stream" in target:
        stream = target["stream"]
        _object(stream, {"buffer", "max_items", "max_bytes"})
        require(
            shape.endswith("_iter")
            and type(stream["buffer"]) is bool
            and type(stream["max_items"]) is int
            and 1 <= stream["max_items"] <= 65536
            and type(stream["max_bytes"]) is int
            and 1 <= stream["max_bytes"] <= 16777216,
            "invalid stream configuration",
        )
    if shape.endswith("_iter"):
        security = hooks.get("security.violation")
        needs_buffer = (
            replaces_result
            or (after and "withhold_result" in after["failure_actions"])
            or (
                security
                and security["mode"] == "control"
                and parse_path(fields["output"]["from"])[0] == "result"
            )
        )
        require(
            not needs_buffer or target.get("stream", {}).get("buffer"),
            "stream requires buffering",
        )
    if custom or "code_sha256" in target:
        require(
            _matches(target.get("code_sha256"), r"[0-9a-f]{64}", 64),
            "invalid code fingerprint",
        )


def _reject_constant(value: str) -> Any:
    raise ValueError(f"unsupported JSON constant {value}")


def _validate_json_tree(value: Any, depth: int = 0) -> None:
    if depth > 32:
        raise ValueError("document is too deep")
    if type(value) is dict:
        for key, item in value.items():
            if not isinstance(key, str) or not key.isascii():
                raise ValueError("invalid metadata key")
            _validate_json_tree(item, depth + 1)
    elif type(value) is list:
        for item in value:
            _validate_json_tree(item, depth + 1)
    elif type(value) is int:
        if abs(value) > 9007199254740991:
            raise ValueError("unsafe metadata integer")
    elif value is not None and type(value) not in (str, bool):
        raise ValueError("unsupported metadata value")


def parse_profile(document: bytes | str | dict[str, Any]) -> dict[str, Any]:
    """Parse the Python profile contract, including canonical AW metadata limits."""
    try:
        if isinstance(document, dict):
            _validate_json_tree(document)
            encoded = json.dumps(
                document, ensure_ascii=False, separators=(",", ":")
            ).encode("utf-8")
        else:
            encoded = (
                document.encode("utf-8") if isinstance(document, str) else document
            )
        if not isinstance(encoded, bytes) or len(encoded) > MAX_PROFILE_BYTES:
            raise ValueError("invalid document size or type")
        value = json.loads(
            encoded.decode("utf-8"),
            object_pairs_hook=object_fields,
            parse_constant=_reject_constant,
        )
        _validate_json_tree(value)
        # Escaped lone surrogates have no UTF-8 representation.
        json.dumps(value, ensure_ascii=False).encode("utf-8")
    except (ValueError, TypeError, RecursionError) as error:
        raise AwError("source_profile_invalid") from error

    _object(value, {"version", "id", "origin", "runtime", "package", "targets"})
    require(value["version"] == VERSION, "unknown profile version")
    require(value["runtime"] == "python", "profile runtime must be python")
    require(value["origin"] in ("builtin", "custom"), "invalid origin")
    require(_matches(value["id"], r"[A-Za-z0-9._-]+", 128), "invalid profile id")
    package = value["package"]
    _object(package, {"name", "versions"})
    require(
        _matches(package["name"], r"[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?", 128),
        "invalid Python package name",
    )
    versions = package["versions"]
    require(
        isinstance(versions, str)
        and 0 < len(versions) <= 128
        and len(versions.split(",")) <= 8
        and all(COMPARATOR.fullmatch(part) for part in versions.split(",")),
        "invalid version range",
    )
    targets = value["targets"]
    require(type(targets) is list and 1 <= len(targets) <= 32, "invalid target count")
    for target in targets:
        _validate_target(target, value["origin"] == "custom")
    require(len({t["id"] for t in targets}) == len(targets), "duplicate target id")
    require(
        len({(t["module"], t["qualname"]) for t in targets}) == len(targets),
        "duplicate target seam",
    )
    return value


def compile_hooks(targets: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    """Intersect authority, effects and failure actions per event as in Rust R16."""
    merged: dict[str, dict[str, Any]] = {}
    for target in targets:
        for event, cap in target["hooks"].items():
            hook = merged.get(event)
            if hook is None:
                merged[event] = {
                    "mode": cap["mode"],
                    "effects": set(cap["effects"]),
                    "failure_actions": set(cap["failure_actions"]),
                }
                continue
            hook["effects"].intersection_update(cap["effects"])
            hook["failure_actions"].intersection_update(cap["failure_actions"])
            if cap["mode"] == "observe":
                hook["mode"] = "observe"
    for hook in merged.values():
        if hook["mode"] == "observe":
            hook["effects"].intersection_update({"observe"})
            hook["failure_actions"].intersection_update({"report"})
        require("observe" in hook["effects"], "merged Hook capability lost observe")
        hook["effects"] = [
            effect
            for effect in ("observe", "block", "replace_input", "replace_result", "ask")
            if effect in hook["effects"]
        ]
        hook["failure_actions"] = [
            action
            for action in ("report", "block", "withhold_result")
            if action in hook["failure_actions"]
        ]
    return merged


def _release(version: str) -> tuple[int, ...]:
    segments = [int(part) for part in version.split(".")]
    while segments and segments[-1] == 0:
        segments.pop()
    return tuple(segments)


def version_matches(installed: str, versions: str) -> bool:
    """Compare PEP 440 release segments numerically, padding with implicit zeros."""
    if not isinstance(installed, str):
        return False
    match = INSTALLED_VERSION.fullmatch(installed.strip())
    if not match:
        return False
    try:
        if match[1] and int(match[1]):
            return False
        actual = _release(match[2])
        for comparator in versions.split(","):
            expected = COMPARATOR.fullmatch(comparator)
            if expected is None or not COMPARISONS[expected[1]](
                actual, _release(expected[2])
            ):
                return False
    except ValueError:
        return False
    return True


def _read(value: Any, segment: str) -> Any:
    if type(value) is dict:
        return value.get(segment, MISSING)
    if type(value) in (list, tuple) and segment.isdecimal():
        index = int(segment)
        return value[index] if index < len(value) else MISSING
    if value is MISSING or not re.fullmatch(IDENT, segment):
        return MISSING
    attribute = getattr_static(value, segment, MISSING)
    if type(attribute) is MemberDescriptorType:
        try:
            return attribute.__get__(value, type(value))
        except AttributeError:
            return MISSING
    if getattr_static(type(attribute), "__get__", MISSING) is not MISSING:
        return MISSING
    return attribute


def _write(value: Any, parts: tuple[str, ...], replacement: Any) -> Any:
    """Rebuild the path from plain containers without invoking application copy hooks."""
    if not parts:
        return replacement
    first, *rest = parts
    current = _read(value, first)
    if current is MISSING:
        raise AwError("source_profile_field_missing")
    updated = _write(current, tuple(rest), replacement)
    if type(value) is dict:
        return {**value, first: updated}
    if type(value) in (list, tuple):
        items = list(value)
        items[int(first)] = updated
        return tuple(items) if type(value) is tuple else items
    raise AwError("source_profile_field_write_unsupported")


def _json_snapshot(value: Any) -> Any:
    if not is_plain_json(value):
        raise AwError("source_invalid_payload")
    return deepcopy(value)


def decode(value: Any, codec: str) -> Any:
    if codec in ("text", "json_string") and not isinstance(value, str):
        raise AwError("source_profile_text_required")
    if codec == "text":
        return value
    if codec == "json_string":
        try:
            value = json.loads(
                value, object_pairs_hook=object_fields, parse_constant=_reject_constant
            )
        except (ValueError, RecursionError) as error:
            raise AwError("source_invalid_payload") from error
    return _json_snapshot(value)


def encode(value: Any, codec: str) -> Any:
    if codec == "text":
        return decode(value, codec)
    value = _json_snapshot(value)
    if codec == "json_string":
        return json.dumps(value, ensure_ascii=False, allow_nan=False)
    return value


@dataclass
class CallFields:
    """Resolve signature slots across positional/keyword calls, excluding the receiver.

    With no inspectable signature, use actual positional or keyword arguments.
    Paths read dict keys, sequence indices and stored attributes without invoking
    properties or custom descriptors.
    """

    signature: Signature | None
    receiver: bool
    args: tuple[Any, ...]
    kwargs: dict[str, Any]

    def _positional(self, index: int) -> tuple[Any, Callable[[Any], None]]:
        def replace(value: Any) -> None:
            self.args = (*self.args[:index], value, *self.args[index + 1 :])

        return (self.args[index] if index < len(self.args) else MISSING), replace

    def _keyword(
        self, key: str, default: Any = MISSING
    ) -> tuple[Any, Callable[[Any], None]]:
        def replace(value: Any) -> None:
            self.kwargs = {**self.kwargs, key: value}

        return self.kwargs.get(key, default), replace

    def _slot(self, root: str) -> tuple[Any, Callable[[Any], None]]:
        if root == "receiver" and not self.receiver:
            raise AwError("source_profile_field_missing")
        kind, _, selector = root.partition(".")
        index = None
        if kind == "receiver":
            index = 0
        elif kind == "arg":
            index = int(selector) + int(self.receiver)
        if self.signature is None:
            return (
                self._positional(index)
                if index is not None
                else self._keyword(selector)
            )

        bound = self.signature.bind(*self.args, **self.kwargs)
        bound.apply_defaults()
        positional = [
            p.name
            for p in self.signature.parameters.values()
            if p.kind in (Parameter.POSITIONAL_ONLY, Parameter.POSITIONAL_OR_KEYWORD)
        ]
        if index is not None:
            if index < len(self.args) or index >= len(positional):
                return self._positional(index)
            key = positional[index]
        else:
            key = selector
            parameter = self.signature.parameters.get(key)
            if (
                key in self.kwargs
                or parameter is None
                or parameter.kind
                not in (Parameter.POSITIONAL_OR_KEYWORD, Parameter.KEYWORD_ONLY)
            ):
                return self._keyword(key)
            if key in positional and positional.index(key) < len(self.args):
                return self._positional(positional.index(key))
        if self.signature.parameters[key].kind != Parameter.POSITIONAL_ONLY:
            return self._keyword(key, bound.arguments[key])

        def replace_default(value: Any) -> None:
            # Earlier positional-only defaults must precede the selected slot.
            self.args = (*bound.args[:index], value)

        return bound.arguments[key], replace_default

    def read(
        self, binding: dict[str, str], result: Any = MISSING, *, nullable: bool = False
    ) -> Any:
        if "const" in binding:
            return binding["const"]
        root, parts = parse_path(binding["from"])
        value = result if root == "result" else self._slot(root)[0]
        for part in parts:
            value = _read(value, part)
        if nullable and (value is None or value is MISSING):
            return None
        if value is MISSING:
            raise AwError("source_profile_field_missing")
        return decode(value, binding["codec"])

    def isolate_input(self, binding: dict[str, str]) -> None:
        root, parts = parse_path(binding["from"])
        value, store = self._slot(root)
        leaf = value
        for part in parts:
            leaf = _read(leaf, part)
        if leaf is MISSING:
            raise AwError("source_profile_field_missing")
        store(_write(value, parts, snapshot_input_arg(leaf, None)))

    def write_input(self, binding: dict[str, str], replacement: Any) -> None:
        root, parts = parse_path(binding["from"])
        value, store = self._slot(root)
        store(_write(value, parts, encode(replacement, binding["codec"])))

    def write_result(
        self, binding: dict[str, str], result: Any, replacement: Any
    ) -> Any:
        _, parts = parse_path(binding["from"])
        return _write(result, parts, encode(replacement, binding["codec"]))
