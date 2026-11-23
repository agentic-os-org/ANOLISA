"""Run the shared SkillViewer contract through the AgentScope invocation chain."""

from contextlib import redirect_stderr
import importlib.util
import io
import json
from pathlib import Path
import sys
from types import SimpleNamespace
import unittest

from agentscope.agent import Agent
from agentscope.message import ToolCallBlock
from agentscope.state import AgentState
from agentscope.tool import Toolkit

from middleware_support import MODULE
import aw_skill

SPEC = importlib.util.spec_from_file_location(
    "qwen_skill_contract", Path(__file__).resolve().parent.parent / "qwenpaw/native_skill.py"
)
CONTRACT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CONTRACT)


class SkillTests(CONTRACT.SkillTests):
    def failure_log(self):
        return redirect_stderr(io.StringIO())

    def selection_metadata(self):
        return aw_skill.metadata()

    async def test_native_utf8_bom_and_crlf_keep_the_same_checked_content(self):
        for index, prefix, newline in [(0, b"", b"\n"), (1, b"", b"\r\n"), (2, b"\xef\xbb\xbf", b"\n")]:
            directory = self.skill(f"encoding-{index}", "CHECKED_INSTRUCTIONS")
            path = directory / "SKILL.md"
            path.write_bytes(prefix + path.read_bytes().replace(b"\n", newline))
            if prefix:
                # SDK 2.0.6 rejects BOM assets before exposing a Skill object.
                captured, entered, _ = await self.execute([str(directory)], code=2)
                self.assertEqual(captured[0]["aw_skill"], {"error_code": "skill_resolution_error"})
                self.assertFalse(entered)
            else:
                captured, entered, result = await self.execute([str(directory)])
                self.assertEqual(captured[0]["aw_skill"]["directory"], str(directory))
                self.assertTrue(entered)
                self.assertIn("CHECKED_INSTRUCTIONS", json.dumps([item.model_dump(mode="json") for item in result]))

    async def execute(self, skills, code=0, before=None, name="example", enabled=True, on_error="block"):
        captured, entered = [], []

        class Probe(MODULE.AwToolMiddleware):
            async def _invoke(self, payload):
                captured.append(payload)
                if before:
                    await before(agent, payload)
                return code, b"{}", b"denied" if code == 2 else b""

        hook = {"event": "tool.before", "step": "skill", "budget_ms": 1000,
                "on_error": on_error, "skill_check": enabled}
        middleware = Probe({}, hook)
        # The invocation wrapper owns one snapshot across every policy and native
        # call; no service registration is needed for this actual Toolkit test.
        owner = MODULE.AwInvocationMiddleware()
        call = ToolCallBlock(id="call", name="Skill", input=json.dumps({"skill": name}))
        agent = SimpleNamespace(toolkit=Toolkit(skills_or_loaders=skills),
                                state=AgentState(session_id="session"),
                                _acting_middlewares=[owner, middleware])

        async def native(tool_call):
            entered.append(True)
            async for result in agent.toolkit.call_tool(tool_call, agent.state):
                yield result

        agent._acting_impl = native
        results = [item async for item in Agent._acting(agent, call)]
        self.assertEqual(aw_skill.metadata(), {})
        return captured, entered, results


if __name__ == "__main__":
    unittest.main()
