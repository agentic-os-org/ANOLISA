"""One real AgentScope Agent accepts turns across AW policy revisions."""
import asyncio
import json
import os
from pathlib import Path

from agentscope.agent import InjectionConfig
from agentscope.formatter import OpenAIChatFormatter
from agentscope.message import TextBlock, ToolCallBlock, UserMsg
from agentscope.model import ChatModelBase, ChatResponse
from agentscope.permission import PermissionBehavior, PermissionDecision
from agentscope.state import AgentState
from agentscope.tool import ToolBase, Toolkit, ToolResponse
from aw_agentscope import AwBridge

ROOT = Path(os.environ["AW_TEST_ROOT"])


async def until(path):
    async with asyncio.timeout(30):
        while not path.exists():
            await asyncio.sleep(.02)


class Model(ChatModelBase):
    def __init__(self):
        super().__init__(None, "fixture", self.Parameters(), stream=False)
        self.formatter = OpenAIChatFormatter()
        self.round = 0
        self.mode = "allow"
        self.turn = 0

    async def __call__(self, *args, **kwargs):
        self.round += 1
        if self.round == 1:
            (ROOT / f"model-{self.turn}").touch()
            if self.mode == "hold-model":
                await until(ROOT / "release-model")
            content = [ToolCallBlock(id="native-call", name="local_tool", input=json.dumps({"mode": self.mode}))]
        else:
            content = [TextBlock(text="done")]
        return ChatResponse(content=content, is_last=True)

    async def count_tokens(self, *args, **kwargs):
        return 1


class Tool(ToolBase):
    name = "local_tool"
    description = "Controlled local operation"
    is_read_only = True
    is_concurrency_safe = True
    input_schema = {"type": "object", "properties": {"mode": {"type": "string"}}, "required": ["mode"]}

    async def check_permissions(self, tool_input, context):
        return PermissionDecision(behavior=PermissionBehavior.ALLOW, message="fixture")

    async def call(self, mode):
        with (ROOT / "executed.jsonl").open("a") as output:
            output.write(json.dumps({"mode": mode}) + "\n")
        if mode == "hold-tool":
            (ROOT / "tool-entered").touch()
            await until(ROOT / "release-tool")
        yield ToolResponse(content=[TextBlock(text="native result")])


async def main():
    model = Model()
    toolkit = Toolkit(tools=[Tool()])
    agent = AwBridge().create_agent(
        name="fixture", system_prompt="Run the local fixture tool", model=model, toolkit=toolkit,
        state=AgentState(session_id="one-resident-session"),
        injection_config=InjectionConfig(inject_runtime_state=False),
    )
    (ROOT / "worker-ready").write_text(str(os.getpid()))
    for index in range(7):
        path = ROOT / f"turn-{index}.json"
        await until(path)
        request = json.loads(path.read_text())
        model.round, model.mode, model.turn = 0, request["mode"], index
        try:
            await agent.reply(UserMsg("user", request.get("text", "hello")))
            outcome = "ok"
        except Exception as error:
            outcome = type(error).__name__
        (ROOT / f"result-{index}.json").write_text(json.dumps({"outcome": outcome, "rounds": model.round}))
    await until(ROOT / "stop-worker")


if __name__ == "__main__":
    asyncio.run(main())
