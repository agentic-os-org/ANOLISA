"""Constrain a gateway-backed Codex 0.166.0 stdio child to fresh local threads."""

from __future__ import annotations

import asyncio
import json
import os
import signal
import stat
import sys
from contextlib import ExitStack, suppress
from typing import Any, Awaitable, Callable

LIMIT = 4 * 1024 * 1024


def thread_start(params: dict[str, Any], provider: str) -> bool:
    if params.keys() - {"cwd", "model", "modelProvider", "ephemeral", "config",
                        "developerInstructions"}:
        return False
    if "cwd" in params and params["cwd"] != os.getcwd():
        return False
    if params.get("modelProvider", provider) != provider:
        return False
    config = params.get("config", {})
    if not isinstance(config, dict) or config.keys() - {"model_context_window", "model_reasoning_effort"}:
        return False
    window = config.get("model_context_window", 1)
    return (type(window) is int and window > 0
            and config.get("model_reasoning_effort", "medium") in ("minimal", "low", "medium", "high", "xhigh"))


def plain_text(item: Any) -> bool:
    return (isinstance(item, dict)
            and not item.keys() - {"type", "text", "text_elements"}
            and item.get("type") == "text"
            and isinstance(item.get("text"), str)
            and item.get("text_elements", []) == [])


class Session:
    def __init__(self, provider: str) -> None:
        self.provider = provider
        self.pending: dict[int | str, str] = {}
        self.threads: set[str] = set()

    def admit(self, frame: dict[str, Any]) -> bool:
        method = frame.get("method")
        params = frame.get("params", {})
        valid_envelope = isinstance(params, dict) and not frame.keys() - {"id", "method", "params", "jsonrpc"}
        if "id" not in frame:
            if method == "initialized" and valid_envelope and not params:
                return True
            raise ValueError("invalid client notification")
        request = frame["id"]
        if type(request) not in (int, str) or request in self.pending:
            raise ValueError("invalid or duplicate client request ID")
        if not valid_envelope or len(self.pending) >= 128:
            return False
        thread = params.get("threadId")
        known_thread = isinstance(thread, str) and thread in self.threads
        if method == "thread/start":
            allowed = len(self.threads) + len(self.pending) < 1024 and thread_start(params, self.provider)
        elif method == "turn/start":
            allowed = (not params.keys() - {"threadId", "input"}
                       and known_thread
                       and isinstance(params.get("input"), list)
                       and all(plain_text(item) for item in params["input"]))
        elif method == "turn/interrupt":
            allowed = (params.keys() == {"threadId", "turnId"} and known_thread and isinstance(params["turnId"], str))
        else:
            allowed = method == "initialize" and not params.keys() - {"clientInfo", "capabilities"}
        if allowed:
            self.pending[request] = method
        return allowed

    def received(self, frame: dict[str, Any]) -> None:
        if "method" not in frame and self.pending.pop(frame.get("id"), None) == "thread/start":
            if "error" not in frame:
                thread = frame["result"]["thread"]["id"]
                if not isinstance(thread, str) or not thread:
                    raise ValueError("invalid native thread")
                self.threads.add(thread)


async def serve() -> int:
    loop = asyncio.get_running_loop()
    with ExitStack() as streams:
        stdin = os.fstat(sys.stdin.fileno())
        # Linux epoll cannot register regular files or the null device.
        null_input = stat.S_ISCHR(stdin.st_mode) and stdin.st_rdev == os.stat(os.devnull).st_rdev
        if stat.S_ISREG(stdin.st_mode) or null_input:
            async def readline() -> bytes:
                line = await asyncio.to_thread(sys.stdin.buffer.readline, LIMIT + 1)
                if len(line) > LIMIT and not line.endswith(b"\n"):
                    raise ValueError("input line too long")
                return line
        else:
            reader = asyncio.StreamReader(limit=LIMIT)
            transport, _ = await loop.connect_read_pipe(
                lambda: asyncio.StreamReaderProtocol(reader), sys.stdin.buffer
            )
            streams.callback(transport.close)
            readline = reader.readline

        if stat.S_ISREG(os.fstat(sys.stdout.fileno()).st_mode):
            async def emit(data: bytes) -> None:
                sys.stdout.buffer.write(data)
                sys.stdout.buffer.flush()

            return await relay(readline, emit)

        pipe, protocol = await loop.connect_write_pipe(asyncio.streams.FlowControlMixin, sys.stdout.buffer)
        writer = asyncio.StreamWriter(pipe, protocol, None, loop)
        streams.callback(writer.close)

        async def emit(data: bytes) -> None:
            writer.write(data)
            await writer.drain()

        status = await relay(readline, emit)
        # At shutdown drain must wait for every byte, including a partial last frame.
        pipe.set_write_buffer_limits(high=0)
        await asyncio.wait_for(writer.drain(), 5)
        return status


async def relay(readline: Callable[[], Awaitable[bytes]], emit: Callable[[bytes], Awaitable[None]]) -> int:
    child = await asyncio.create_subprocess_exec(*sys.argv[2:], stdin=asyncio.subprocess.PIPE,
                                                stdout=asyncio.subprocess.PIPE, limit=LIMIT)
    session = Session(sys.argv[1])
    # AW signals the owned process group. Reap Codex before the relay exits.
    asyncio.get_running_loop().add_signal_handler(signal.SIGTERM, asyncio.current_task().cancel)

    client_eof = False

    def close_stdin_if_drained() -> None:
        if client_eof and not session.pending:
            child.stdin.close()

    async def inputs() -> None:
        nonlocal client_eof
        while line := await readline():
            frame = json.loads(line)
            if not session.admit(frame):
                await emit(json.dumps({"id": frame["id"], "error": {"code": -32602,
                                "message": "Outside AW fixed-thread contract"}}).encode() + b"\n")
                continue
            # Serialize the inspected object, including its treatment of duplicate keys.
            child.stdin.write(json.dumps(frame, allow_nan=False).encode() + b"\n")
            await child.stdin.drain()
        client_eof = True
        close_stdin_if_drained()

    async def outputs() -> None:
        while line := await child.stdout.readline():
            frame = json.loads(line)
            if "method" in frame and "id" in frame:
                # This profile has no client-side approval or dynamic-tool handlers.
                child.stdin.write(json.dumps({"id": frame["id"], "error": {"code": -32601,
                                  "message": "Unsupported server request"}}).encode() + b"\n")
                await child.stdin.drain()
                continue
            session.received(frame)
            close_stdin_if_drained()
            await emit(line)

    incoming = asyncio.create_task(inputs())
    outgoing = asyncio.create_task(outputs())
    try:
        done, _ = await asyncio.wait((incoming, outgoing), return_when=asyncio.FIRST_COMPLETED)
        for task in done:
            task.result()
        if outgoing not in done:
            await asyncio.wait_for(outgoing, 5)
        status = await asyncio.wait_for(child.wait(), 5)
        if status == 0 and session.pending:
            raise ValueError("native process exited with pending replies")
        return status
    finally:
        for task in (incoming, outgoing):
            task.cancel()
        await asyncio.gather(incoming, outgoing, return_exceptions=True)
        if child.returncode is None:
            with suppress(ProcessLookupError):
                child.kill()
        # Drain buffered stdout so a full subprocess pipe cannot stall reaping.
        await asyncio.wait_for(child.communicate(), 5)


if __name__ == "__main__":
    try:
        status = asyncio.run(serve())
        sys.exit(128 - status if status < 0 else status)
    except asyncio.CancelledError:
        sys.exit(128 + signal.SIGTERM)
    except Exception as error:
        # Frames may contain prompts, credentials or tool results.
        print(f"AW Codex stdio closed: {type(error).__name__}", file=sys.stderr)
        sys.exit(1)
