"""Exercise SDK tool dispatch with synthetic calls; no model or native hook is used."""

import argparse
import asyncio
from importlib.metadata import version
import json
import os
from pathlib import Path
import subprocess
from typing import Any


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "sdk", choices=["langchain", "langchain_async", "langchain_direct", "claude"]
    )
    parser.add_argument("mode", choices=["exec", "file", "memory", "partial", "inherited_fd"])
    parser.add_argument("marker", type=Path)
    parser.add_argument("fd", type=int)
    args = parser.parse_args()
    state: dict[str, Any] = {"entered": 0, "memory_effect": 0, "operation_error": None}

    def effect() -> str:
        """Apply a synthetic operation to the owned fixture."""
        state["entered"] += 1
        try:
            if args.mode in ("memory", "partial"):
                state["memory_effect"] += 1
            if args.mode in ("file", "partial"):
                args.marker.write_text("synthetic")
            if args.mode == "exec":
                subprocess.run(
                    ["/bin/sh", "-c", 'printf synthetic > "$1"', "fixture", str(args.marker)],
                    check=True,
                    timeout=3,
                )
            if args.mode == "inherited_fd":
                os.write(args.fd, b"synthetic")
        except PermissionError:
            state["operation_error"] = "EPERM"
        return json.dumps(state)

    if args.sdk.startswith("langchain"):
        from langchain_core.tools import tool

        function = tool(effect)
        if args.sdk == "langchain_async":
            state["dispatch_result"] = await function.ainvoke({})
        elif args.sdk == "langchain_direct":
            state["dispatch_result"] = function.func()
        else:
            state["dispatch_result"] = function.invoke({})
        package = "langchain-core"
    else:
        from claude_agent_sdk import create_sdk_mcp_server, tool
        from mcp.types import CallToolRequest

        @tool("effect", "Synthetic effect", {})
        async def function(arguments: dict[str, Any]) -> dict[str, Any]:
            return {"content": [{"type": "text", "text": effect()}]}

        server = create_sdk_mcp_server(name="boundary", tools=[function])["instance"]
        request = CallToolRequest(method="tools/call", params={"name": "effect", "arguments": {}})
        reply = await server.request_handlers[CallToolRequest](request)
        if reply.root.isError:
            raise RuntimeError("SDK dispatch failed")
        state["dispatch_result"] = reply.root.content[0].text
        package = "claude-agent-sdk"
    print(
        json.dumps(
            {
                "sdk": package,
                "version": version(package),
                "mode": args.mode,
                "no_new_privs": "NoNewPrivs:\t1" in Path("/proc/self/status").read_text(),
                **state,
            }
        )
    )


if __name__ == "__main__":
    asyncio.run(main())
