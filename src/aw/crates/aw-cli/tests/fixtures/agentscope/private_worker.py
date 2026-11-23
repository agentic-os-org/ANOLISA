"""Private tool credentials in a real AgentScope factory and HTTP model loop."""
import asyncio
from contextlib import aclosing, asynccontextmanager
import json
import os
from pathlib import Path

from agentscope.agent import InjectionConfig
from agentscope.credential import OpenAICredential
from agentscope.event import ToolResultTextDeltaEvent
from agentscope.message import TextBlock, UserMsg
from agentscope.model import OpenAIChatModel
from agentscope.permission import PermissionBehavior, PermissionDecision
from agentscope.state import AgentState
from agentscope.tool import ToolBase, ToolChunk, Toolkit
from aw_agentscope import AwBridge
from aw_private import PrivateExecution, execution_environment

ROOT = Path(os.environ["AW_TEST_ROOT"])
SECRET = "synthetic-task-private-value"


async def until(path):
    async with asyncio.timeout(30):
        while not path.exists():
            await asyncio.sleep(.02)


def record(name, value):
    with (ROOT / name).open("a") as output:
        output.write(json.dumps(value) + "\n")


class Tool(ToolBase):
    name = "private_tool"
    description = "Execute with a task-owned environment"
    is_read_only = True
    is_concurrency_safe = True
    input_schema = {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}

    async def check_permissions(self, tool_input, context):
        return PermissionDecision(behavior=PermissionBehavior.ALLOW, message="fixture")

    async def call(self, text):
        assert text == "unchanged native arguments"
        assert "AW_PRIVATE_KEY" not in os.environ
        environment = execution_environment()
        # The real tool subprocess explicitly consumes its lease.
        process = await asyncio.create_subprocess_exec("/bin/sh", "-c", 'printf %s "$AW_PRIVATE_KEY"',
            env=environment, stdout=asyncio.subprocess.PIPE)
        stdout, _ = await process.communicate()
        assert process.returncode == 0 and stdout.decode() == SECRET
        record("tools.jsonl", {"native_arguments": text, "credential_present": True})
        yield ToolChunk(content=[TextBlock(id="split", text=SECRET[:10])], is_last=False)
        yield ToolChunk(content=[TextBlock(id="split", text=SECRET[10:])], is_last=True,
                        metadata={"credential": SECRET})


async def main():
    mode = "normal"

    @asynccontextmanager
    async def factory():
        record("leases.jsonl", {"action": "acquire", "mode": mode})
        if mode == "acquire":
            (ROOT / "acquiring").touch()
            await until(ROOT / "allow-acquire")
        item = PrivateExecution({}, {"PUBLIC": "visible"},
            lambda value: json.loads(json.dumps(value).replace(SECRET, "[redacted]")),
            {"AW_PRIVATE_KEY": SECRET})
        try:
            yield item
        finally:
            record("leases.jsonl", {"action": "revoke", "mode": mode})
            if mode == "revoke":
                (ROOT / "revoking").touch()
                await until(ROOT / "allow-revoke")
            if mode == "fail":
                raise RuntimeError(SECRET)
            record("leases.jsonl", {"action": "revoked", "mode": mode})

    agent = AwBridge().create_agent(name="fixture", system_prompt="Run private_tool",
        model=OpenAIChatModel(OpenAICredential(api_key="fixture", base_url=os.environ["AW_TEST_MODEL"]),
                              model="fixture", stream=False, max_retries=0, context_size=128000),
        toolkit=Toolkit(tools=[Tool()]), state=AgentState(session_id="private-session"),
        injection_config=InjectionConfig(inject_runtime_state=False), execution_factory=factory)
    (ROOT / "ready").touch()
    for index in range(6):
        request = ROOT / f"turn-{index}"
        await until(request)
        mode = request.read_text()
        try:
            if mode == "stream":
                deltas = []
                async with aclosing(agent.reply_stream(UserMsg("user", mode))) as stream:
                    async for item in stream:
                        assert SECRET not in str(item)
                        if isinstance(item, ToolResultTextDeltaEvent):
                            deltas.append(item.delta)
                assert deltas == ["[redacted]"], deltas
            else:
                await agent.reply(UserMsg("user", mode))
            outcome = "ok"
        except Exception as error:
            assert SECRET not in str(error)
            outcome = type(error).__name__
        (ROOT / f"result-{index}").write_text(outcome)
    await until(ROOT / "stop")


if __name__ == "__main__":
    asyncio.run(main())
