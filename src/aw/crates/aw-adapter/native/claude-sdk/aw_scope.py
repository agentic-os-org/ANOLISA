"""Bounded CLI transport and acknowledged, process-local SDK task handles."""

from __future__ import annotations

import asyncio
import json
import os
from pathlib import Path
import signal
import sys
from typing import Any

import anyio


async def command(
    config: dict[str, Any],
    action: str,
    value: Any,
    timeout: float,
    hook: dict[str, Any] | None = None,
    environment: dict[str, str] | None = None,
) -> tuple[int, bytes]:
    """Keep service RPC and binding serialization inside AW's CLI boundary."""
    encoded = json.dumps(value, ensure_ascii=False, allow_nan=False).encode()
    if len(encoded) > 1024 * 1024:
        raise RuntimeError("SDK scope input exceeds one MiB")
    args = [
        config["executable"],
        "scope",
        "--adapter",
        "claude-sdk",
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
                raise RuntimeError("SDK scope response exceeds its byte limit")
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
    if stderr:
        sys.stderr.buffer.write(stderr)
        sys.stderr.buffer.flush()
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

    def check(self) -> None:
        if self.pid != os.getpid() or self.closed or self.handle is None:
            raise RuntimeError("SDK scope is closed, unbound or inherited across fork")

    async def open(self, cwd: str, environment: dict[str, str]) -> None:
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
        self.check()
        return await command(
            self.config,
            "invoke",
            {
                "scope": self.handle,
                "native_input": json.dumps(payload, ensure_ascii=False, allow_nan=False),
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
