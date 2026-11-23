"""Instrument an arbitrary Python agent with zero dedicated Rust Adapter."""

import asyncio
import os
from aw_sdk.sources import LanguageSource, SourceDenied


class CustomAgent:
    """Example application-owned agent with ordinary Python methods."""

    async def ask(self, prompt: str) -> str:
        return await self.echo(text=prompt)

    async def echo(self, *, text: str) -> str:
        return f"echo:{text}"


async def main() -> None:
    agent = CustomAgent()
    async with LanguageSource.create(
        endpoint=os.environ["AW_SOCKET"],
        target="app",
        availability="required",
    ) as source:
        async with source.session(session_id="example-python-agent") as session:
            session.instrument(
                agent,
                input_methods=["ask"],
                tool_methods={"echo": "echo"},
            )
            try:
                print(await agent.ask("hello"))
            except SourceDenied as denied:
                print(f"AW denied {denied.event}")


asyncio.run(main())
