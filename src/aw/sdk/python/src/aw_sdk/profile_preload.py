"""Private startup entrypoint for an AW-provisioned Python profile binding."""

from __future__ import annotations

import asyncio
import atexit
import json
import os
import stat
from collections.abc import Coroutine
from pathlib import Path
from threading import Event, Thread
from typing import Any

from aw_sdk._profile_bootstrap import configured_payload, verify_capabilities
from aw_sdk._profile_eval import (
    MAX_PROFILE_BYTES,
    _reject_constant,
    _validate_json_tree,
)
from aw_sdk._profile_process import ProcessProfile, _Scope
from aw_sdk.deployment import SDK_PROFILE, _read_file, load_descriptor, object_fields
from aw_sdk.transport import AwError

preloaded_profile_source: ProcessProfile | None = None


def _bootstrap(path: str) -> dict:
    try:
        data, mode = _read_file(path, MAX_PROFILE_BYTES)
        if stat.S_IMODE(mode) not in (0o400, 0o600):
            raise ValueError()
        value = json.loads(
            data.decode("utf-8"),
            object_pairs_hook=object_fields,
            parse_constant=_reject_constant,
        )
        _validate_json_tree(value)
        if (
            not isinstance(value, dict)
            or ("profile" in value) == ("profiles" in value)
            or not value.keys()
            <= {
                "profile",
                "profiles",
                "availability",
                "descriptorFile",
                "configFile",
                "target",
                "endpoint",
                "cooperative",
                "timeoutMs",
                "ready",
            }
            or ("descriptorFile" in value) != ("configFile" in value)
            or ("endpoint" in value and "descriptorFile" in value)
            or ("target" in value and not isinstance(value["target"], str))
            or type(value.get("cooperative", False)) is not bool
            or type(value.get("timeoutMs", 5000)) is not int
            or not 100 <= value.get("timeoutMs", 5000) <= 60000
        ):
            raise ValueError()
        return value
    except (AwError, ValueError, TypeError, RecursionError):
        raise AwError("source_profile_bootstrap_invalid") from None


def _receipt(ready: dict, closed: bool = False) -> None:
    path = Path(ready["path"])
    if closed:
        path = path.with_suffix(".closed")
    payload = {
        "version": 1,
        "adapter": "sdk",
        "pid": os.getpid(),
        "hooks": ready["hooks"],
        "token": ready["token"],
    }
    # Rename a complete receipt so the launcher never observes partial JSON.
    temporary = path.with_suffix(path.suffix + ".tmp")
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as stream:
        json.dump(payload, stream)
    os.replace(temporary, path)


class _Runtime:
    def __init__(self, source: ProcessProfile, timeout_ms: int, ready: dict | None):
        self.source = source
        self.timeout_ms = timeout_ms
        self.ready = ready
        self.loop = asyncio.new_event_loop()
        started = Event()

        def run() -> None:
            self.loop.call_soon(started.set)
            try:
                self.loop.run_forever()
            finally:
                self.loop.close()

        self.thread = Thread(target=run, name="aw-profile", daemon=True)
        self.thread.start()
        if not started.wait(timeout_ms / 1000):
            raise AwError("source_profile_startup_timeout")
        self.scope: _Scope | None = None

    def call(self, hook: Coroutine) -> Any:
        return asyncio.run_coroutine_threadsafe(hook, self.loop).result(
            self.timeout_ms / 1000 + 1
        )

    def start(self, descriptor: dict, cooperative: bool) -> None:
        self.source.cooperative = cooperative
        self.call(
            self.source.activate(
                endpoint=descriptor["endpoint"],
                target=descriptor["target"],
                cooperative=cooperative,
                timeout_ms=self.timeout_ms,
            )
        )
        if not cooperative:
            self.scope = _Scope(self.source)
            self.source.default_request = self.scope.opened.result(
                self.timeout_ms / 1000 + 1
            )
        if self.ready:
            _receipt(self.ready)

    def stop(self) -> None:
        self.source.stopping = True
        try:
            try:
                if self.scope:
                    self.scope.close().result(self.timeout_ms / 1000 + 1)
            finally:
                self.call(self.source.teardown(self.timeout_ms))
            if self.ready:
                _receipt(self.ready, closed=True)
        finally:
            self.loop.call_soon_threadsafe(self.loop.stop)
            self.thread.join(self.timeout_ms / 1000)
            if self.thread.is_alive():
                raise AwError("source_cleanup_unconfirmed")

    def shutdown(self) -> None:
        try:
            self.stop()
        except (Exception, KeyboardInterrupt):  # noqa: BLE001
            os.write(2, b"AW: source_cleanup_unconfirmed\n")
            if self.source.availability == "required":
                os._exit(1)


def preload() -> ProcessProfile | None:
    """Consume the private bootstrap once and optionally start the SDK loop."""
    global preloaded_profile_source
    path = os.environ.pop("AW_PROFILE_BOOTSTRAP", None)
    if path is None:
        return preloaded_profile_source
    value = _bootstrap(path)
    descriptor = None
    availability = value.get("availability", "required")
    if "descriptorFile" in value:
        descriptor = load_descriptor(
            value.get("configFile"),
            value["descriptorFile"],
            SDK_PROFILE,
            value.get("target"),
            requires_service=availability == "required",
        )
        availability = descriptor["availability"]
    elif "endpoint" in value:
        descriptor = {"endpoint": value["endpoint"], "target": value.get("target")}

    if "profiles" in value:
        if descriptor is None:
            raise AwError("source_profile_binding_mismatch")
        profile, *additional = configured_payload(value["profiles"], descriptor)
    else:
        if descriptor and "instrumentation_profiles" in descriptor:
            raise AwError("source_profile_binding_mismatch")
        profile, additional = value["profile"], []
    source = ProcessProfile(
        profile, availability=availability, _additional_profiles=tuple(additional)
    )
    if descriptor:
        verify_capabilities(source, descriptor)
    source.install()
    source.verify()
    if descriptor is not None:
        runtime = _Runtime(source, value.get("timeoutMs", 5000), value.get("ready"))
        try:
            runtime.start(descriptor, value.get("cooperative", False))
        except BaseException:
            runtime.stop()
            raise
        atexit.register(runtime.shutdown)
    preloaded_profile_source = source
    return source
