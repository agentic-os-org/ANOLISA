"""Bounded CLI transport and acknowledged, process-local SDK task handles."""

from __future__ import annotations

import asyncio
import json
import os
from pathlib import Path
import signal
import sys
from typing import Any
from uuid import uuid4

import anyio


class ScopeTransportFailure(RuntimeError):
    """Bounded payload or pipe failures, distinct from scope ownership failures."""


async def exchange(
    config: dict[str, Any],
    action: str,
    value: Any,
    timeout: float,
    hook: dict[str, Any] | None = None,
    environment: dict[str, str] | None = None,
) -> tuple[int, bytes, bytes]:
    """Keep service RPC and binding serialization inside AW's CLI boundary."""
    try:
        encoded = json.dumps(value, ensure_ascii=False, allow_nan=False).encode()
    except (TypeError, ValueError) as error:
        raise ScopeTransportFailure("SDK scope payload cannot be encoded") from error
    if len(encoded) > 1024 * 1024:
        raise ScopeTransportFailure("SDK scope input exceeds one MiB")
    args = [
        config["executable"],
        "scope",
        "--adapter",
        config.get("adapter", "claude-sdk"),
        "--binding",
        config["binding"],
        "--action",
        action,
    ]
    if hook is not None:
        args += ["--event", hook["event"], "--step", hook["step"], "--on-error", hook["on_error"]]
    process = await asyncio.create_subprocess_exec(
        *args,
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        start_new_session=True,
        env=environment,
    )

    async def read(stream: asyncio.StreamReader, limit: int) -> bytes:
        result = bytearray()
        while chunk := await stream.read(65536):
            result.extend(chunk)
            if len(result) > limit:
                raise ScopeTransportFailure("SDK scope response exceeds its byte limit")
        return bytes(result)

    async def write() -> None:
        process.stdin.write(encoded)
        await process.stdin.drain()
        process.stdin.close()

    jobs = [
        asyncio.create_task(operation)
        for operation in (
            read(process.stdout, 1024 * 1024),
            read(process.stderr, 65536),
            write(),
            process.wait(),
        )
    ]
    joined = asyncio.gather(*jobs)
    try:
        stdout, stderr, _, code = await asyncio.wait_for(joined, timeout)
    except BaseException:
        if process.returncode is None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        with anyio.CancelScope(shield=True):
            await process.wait()
        raise
    finally:
        with anyio.CancelScope(shield=True):
            for job in jobs:
                if not job.done():
                    job.cancel()
            await asyncio.gather(joined, *jobs, return_exceptions=True)
    return code, stdout, stderr


def diagnostics(stderr: bytes) -> None:
    if stderr:
        sys.stderr.buffer.write(stderr)
        sys.stderr.buffer.flush()


async def command(
    config: dict[str, Any], action: str, value: Any, timeout: float,
    hook: dict[str, Any] | None = None,
    environment: dict[str, str] | None = None,
) -> tuple[int, bytes]:
    """Keep lifecycle callers' response contract while reporting diagnostics."""
    code, stdout, stderr = await exchange(config, action, value, timeout, hook, environment)
    diagnostics(stderr)
    return code, stdout


class Scope:
    """One task owns a handle from admission through acknowledged release."""

    def __init__(self, config: dict[str, Any]) -> None:
        self.config = config
        self.pid = os.getpid()
        self.handle: dict[str, Any] | None = None
        self.closed = False
        self.release_error: BaseException | None = None
        self.environment: dict[str, str] = {}
        self.invocation_limit = 0
        self.reservation: str | None = None

    def check(self) -> None:
        if self.pid != os.getpid() or self.closed or self.handle is None:
            raise RuntimeError("SDK scope is closed, unbound or inherited across fork")

    async def ready(self) -> None:
        """Confirm managed startup without admitting an application task."""
        if self.reservation is not None or self.handle is not None or self.closed or self.pid != os.getpid():
            raise RuntimeError("AW worker readiness requires an unused scope")
        code, _ = await command(self.config, "ready", {"pid": self.pid}, 6)
        if code:
            raise RuntimeError("AW worker readiness failed")

    async def reserve(self) -> None:
        """Retain root occupancy before application-owned task preparation starts."""
        if self.reservation is not None or self.closed or self.pid != os.getpid():
            raise RuntimeError("AW task reservation cannot be reused or inherited")
        # Keep the key even when the response is lost: only explicit confirmed
        # cleanup may release a reservation whose commit outcome is unknown.
        self.reservation = str(uuid4())
        code, _ = await command(
            self.config, "reserve", {"pid": self.pid, "key": self.reservation}, 6,
        )
        if code:
            raise RuntimeError("AW task reservation failed")

    async def release(self) -> None:
        """Release occupancy only after child and application cleanup succeeds."""
        if self.reservation is None:
            return
        if self.pid != os.getpid() or not self.closed or self.release_error is not None:
            raise RuntimeError("AW task reservation still owns unconfirmed cleanup")
        code, _ = await command(
            self.config, "release", {"pid": self.pid, "key": self.reservation}, 6,
        )
        if code:
            raise RuntimeError("AW task reservation release failed; worker restart required")
        self.reservation = None

    async def policy(self) -> dict[str, Any]:
        """Fetch this task's immutable routes before acknowledging native setup."""
        self.check()
        code, output = await command(self.config, "policy", self.handle, 6)
        if code:
            raise RuntimeError("AW task policy handoff failed")
        return json.loads(output)

    async def open(
        self, cwd: str, environment: dict[str, str], *, acknowledge: bool = True,
    ) -> None:
        if self.handle is not None or self.closed or self.pid != os.getpid():
            raise RuntimeError("SDK scope cannot be reopened or inherited across fork")
        self.environment = dict(environment)
        code, output = await command(
            self.config,
            "open",
            {
                "pid": self.pid,
                "cwd": str(Path(cwd).resolve()),
                "environment": environment,
            },
            33,
        )
        if code:
            raise RuntimeError("AW task admission failed")
        self.handle = json.loads(output)
        # The handle is retained before acknowledgement; cancellation can now release it.
        if acknowledge:
            await self.acknowledge()

    async def acknowledge(self) -> None:
        """Enable callbacks only after the native owner has installed its routes."""
        self.check()
        try:
            code, _ = await command(self.config, "acknowledge", self.handle, 6)
            if code:
                raise RuntimeError("AW task handoff acknowledgement failed")
            code, output = await command(self.config, "enable", self.handle, 6)
            if code:
                raise RuntimeError("AW invocation protocol admission failed")
            self.invocation_limit = json.loads(output)["invocations_per_binding"]
        except BaseException:
            # ACK may already have committed; release despite request cancellation.
            with anyio.CancelScope(shield=True):
                await self.close()
            raise

    async def invoke(self, hook: dict[str, Any], payload: dict[str, Any]) -> tuple[int, bytes]:
        code, stdout, stderr = await self.invoke_native(hook, payload)
        diagnostics(stderr)
        return code, stdout

    async def invoke_native(
        self, hook: dict[str, Any], payload: dict[str, Any],
    ) -> tuple[int, bytes, bytes]:
        """Retain native denial text separately from structured stdout directives."""
        self.check()
        try:
            native_input = json.dumps(payload, ensure_ascii=False, allow_nan=False)
        except (TypeError, ValueError) as error:
            raise ScopeTransportFailure("SDK native payload cannot be encoded") from error
        return await exchange(
            self.config,
            "invoke",
            {
                "scope": self.handle,
                "native_input": native_input,
            },
            (hook["budget_ms"] + 3000) / 1000,
            hook,
            self.environment,
        )

    async def close(self) -> None:
        if self.closed:
            if self.release_error is not None:
                raise RuntimeError(
                    "AW task release failed; worker restart required"
                ) from self.release_error
            return
        if self.pid != os.getpid():
            raise RuntimeError("A forked child cannot close its parent's SDK scope")
        self.closed = True
        if self.handle is not None:
            try:
                code, _ = await command(self.config, "close", self.handle, 6)
                if code:
                    raise RuntimeError("AW task release failed; worker restart required")
            except BaseException as error:
                self.release_error = error
                raise
            finally:
                self.environment.clear()

    async def register(self, sequence: int, call_id: str, name: str, arguments: Any) -> None:
        self.check()
        code, _ = await command(
            self.config,
            "register",
            {
                "scope": self.handle,
                "sequence": sequence,
                "call_id": call_id,
                "tool_name": name,
                "input": arguments,
            },
            6,
        )
        if code:
            raise RuntimeError("AW invocation registration rejected")

    async def finish(self, sequence: int, call_id: str) -> None:
        self.check()
        code, _ = await command(
            self.config,
            "finish",
            {
                "scope": self.handle,
                "sequence": sequence,
                "call_id": call_id,
            },
            6,
        )
        if code:
            raise RuntimeError("AW invocation drain failed")
