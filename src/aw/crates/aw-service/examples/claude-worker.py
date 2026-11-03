"""One foreground Claude SDK task; model authentication remains SDK configuration."""

import asyncio
import os
import sys
from typing import Any

from aw_claude import create_client
from claude_agent_sdk import ClaudeAgentOptions, create_sdk_mcp_server, tool


@tool("echo", "Return the supplied text", {"text": str})
async def echo(arguments: dict[str, Any]) -> dict[str, Any]:
    return {"content": [{"type": "text", "text": arguments["text"]}]}


async def main() -> None:
    options = ClaudeAgentOptions(
        cwd=os.getcwd(),
        tools=["Bash"],
        allowed_tools=["Bash", "mcp__example__echo"],
        setting_sources=[],
        mcp_servers={"example": create_sdk_mcp_server("example", tools=[echo])},
    )
    async with create_client(options) as client:
        await client.query(sys.argv[1] if len(sys.argv) > 1 else "Use echo to return hello.")
        async for message in client.receive_response():
            print(message)


if __name__ == "__main__":
    asyncio.run(main())
