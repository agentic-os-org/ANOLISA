"""Check the built MCP server in a temporary userland namespace."""

import argparse
import asyncio
import contextlib
import json
import math
import os
import sys
import tempfile
from pathlib import Path


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


async def check(binary: Path, timeout: float) -> None:
    with tempfile.TemporaryDirectory(prefix="memory-smoke-") as directory:
        root = Path(directory)
        config = root / "memory.toml"
        config.write_text("", encoding="utf-8")
        (root / "outside").write_text("outside smoke sentinel", encoding="utf-8")
        env = {
            **{
                key: value
                for key, value in os.environ.items()
                if not key.startswith(("MEMORY_", "MCP_")) and key != "RUST_LOG"
            },
            "MEMORY_BASE_DIR": str(root / "store"),
            "MEMORY_SESSION_DIR": str(root / "sessions"),
            "MEMORY_MOUNT_STRATEGY": "userland",
            "MEMORY_EMBEDDING_BACKEND": "none",
            "MEMORY_GIT_ENABLED": "false",
            "USER_ID": "smoke",
        }
        command = [str(binary), "--config", str(config)]
        process = None

        async def drain_stdout() -> bool:
            """Resume even a paused pipe without retaining unbounded output."""
            extra = False
            if process.stdout is not None:
                while await process.stdout.read(65536):
                    extra = True
            return extra

        async def cleanup() -> None:
            await drain_stdout()
            await process.wait()

        async def run() -> None:
            nonlocal process
            for subcommand in ("init", "info"):
                process = await asyncio.create_subprocess_exec(*command, subcommand, env=env)
                require(await process.wait() == 0, f"{subcommand} failed")

            process = await asyncio.create_subprocess_exec(
                *command,
                "serve",
                env=env,
                stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE,
                limit=16 * 1024 * 1024,
            )
            request_id = 0

            async def send(message: dict) -> None:
                process.stdin.write((json.dumps(message) + "\n").encode())
                await process.stdin.drain()

            async def request(method: str, params: dict) -> dict:
                nonlocal request_id
                request_id += 1
                await send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
                while True:
                    line = await process.stdout.readline()
                    require(line, f"server closed stdout during {method}")
                    try:
                        response = json.loads(line)
                    except ValueError as error:
                        raise RuntimeError(
                            f"non-JSON stdout during {method}: {line[:200]!r}"
                        ) from error
                    require(isinstance(response, dict), "response must be an object")
                    require(response.get("jsonrpc") == "2.0", "invalid JSON-RPC version")
                    if "id" not in response and "method" in response:
                        continue
                    require(response.get("id") == request_id, "response ID mismatch")
                    require("error" not in response, f"{method}: {response.get('error')}")
                    result = response.get("result")
                    require(isinstance(result, dict), f"{method}: missing result")
                    return result

            async def tool(name: str, arguments: dict, expected_error: bool = False) -> str:
                result = await request("tools/call", {"name": name, "arguments": arguments})
                require(
                    bool(result.get("isError", False)) == expected_error,
                    f"{name}: unexpected tool error status: {result}",
                )
                return "".join(
                    item["text"] for item in result.get("content", []) if item.get("type") == "text"
                )

            initialized = await request(
                "initialize",
                {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "smoke", "version": "1.0"},
                },
            )
            require(
                initialized.get("protocolVersion") == "2024-11-05", "unexpected protocol version"
            )
            await send({"jsonrpc": "2.0", "method": "notifications/initialized"})
            listing = await request("tools/list", {})
            names = {item["name"] for item in listing.get("tools", [])}
            require(
                {"mem_write", "mem_read", "mem_append", "mem_grep", "mem_list"} <= names,
                "required smoke tools are missing",
            )
            path = "notes/smoke.md"
            await tool("mem_write", {"path": path, "content": "hello smoke world"})
            require(
                await tool("mem_read", {"path": path}) == "hello smoke world",
                "read did not return the written content",
            )
            await tool("mem_append", {"path": path, "content": "\nappended smoke"})
            require(
                await tool("mem_read", {"path": path}) == "hello smoke world\nappended smoke",
                "append did not preserve the expected content",
            )
            hits = json.loads(await tool("mem_grep", {"pattern": "smoke"}))
            require(any(hit.get("path") == path for hit in hits), "grep missed the smoke file")
            entries = json.loads(await tool("mem_list", {"recursive": True}))
            require(
                any(entry.get("path") == path for entry in entries), "list missed the smoke file"
            )
            await tool("mem_read", {"path": "../../outside"}, expected_error=True)
            process.stdin.close()
            extra_output = await drain_stdout()
            require(await process.wait() == 0, "server exited unsuccessfully")
            require(not extra_output, "unexpected stdout after the protocol completed")

        try:
            await asyncio.wait_for(run(), timeout)
        finally:
            if process is not None:
                if process.returncode is None:
                    with contextlib.suppress(ProcessLookupError):
                        process.kill()
                # run() has finished or been cancelled, so no other stdout reader remains.
                # Drain before waiting: Python 3.11+ waiters can depend on pipe EOF.
                await asyncio.wait_for(cleanup(), 2)
    print("==> Smoke test PASSED")


def main() -> None:
    if sys.version_info < (3, 8):
        sys.exit("Smoke test requires Python 3.8 or newer")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--timeout", type=float, default=30)
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("--timeout must be a finite positive number")
    try:
        asyncio.run(check(args.binary.resolve(), args.timeout))
    except (
        OSError,
        ValueError,
        RuntimeError,
        KeyError,
        TypeError,
        AttributeError,
        asyncio.TimeoutError,
    ) as error:
        reason = "timed out" if isinstance(error, asyncio.TimeoutError) else str(error)
        parser.exit(1, f"Smoke test FAILED: {reason}\n")


if __name__ == "__main__":
    main()
