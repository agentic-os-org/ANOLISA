"""Actual AgentScope Toolkit selection and SkillViewer denial/snapshot contracts."""

import asyncio
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest

from agentscope.message import ToolCallBlock
from agentscope.skill import Skill, LocalSkillLoader
from agentscope.state import AgentState
from agentscope.tool import Toolkit

ROOT = Path(__file__).resolve().parents[5]
SPEC = importlib.util.spec_from_file_location("aw_skill_test", ROOT / "crates/aw-adapter/native/qwenpaw/plugin.py",
    submodule_search_locations=[str(ROOT / "crates/aw-adapter/native/qwenpaw")])
PLUGIN = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = PLUGIN
SPEC.loader.exec_module(PLUGIN)


class SkillTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def skill(self, directory, body):
        path = self.root / directory
        path.mkdir()
        (path / "SKILL.md").write_text(f"---\nname: example\ndescription: fixture\n---\n{body}\n")
        return path

    async def execute(self, skills, code=0, before=None, name="example", enabled=True, on_error="block"):
        agent = SimpleNamespace(toolkit=Toolkit(skills_or_loaders=skills), state=AgentState(session_id="session"))
        call = ToolCallBlock(id="call", name="Skill", input=json.dumps({"skill": name}))
        captured, entered = [], []
        class Probe(PLUGIN.AwToolMiddleware):
            async def _invoke(self, payload):
                captured.append(payload)
                if before:
                    await before(agent, payload)
                return code, b"{}", b"denied" if code == 2 else b""
        middleware = Probe({}, {"event":"tool.before", "step":"skill", "budget_ms":1000, "on_error":on_error, "skill_check":enabled})
        async def native():
            entered.append(True)
            async for result in agent.toolkit.call_tool(call, agent.state):
                yield result
        results = [item async for item in middleware.on_acting(agent, {"tool_call":call}, native)]
        return captured, entered, results

    async def test_actual_toolkit_duplicate_precedence_and_manifest_names(self):
        first = self.skill("first", "FIRST")
        second = self.skill("second", "SECOND")
        captured, entered, result = await self.execute([str(first), str(second)])
        self.assertEqual(captured[0]["aw_skill"], {"name":"example", "directory":str(second), "source":"toolkit"})
        self.assertTrue(entered)
        text = json.dumps([item.model_dump(mode="json") for item in result])
        self.assertIn("SECOND", text)
        self.assertNotIn("FIRST", text)

    async def test_denial_never_enters_native_viewer(self):
        directory = self.skill("denied", "PRIVATE_SKILL_INSTRUCTIONS")
        _, entered, result = await self.execute([str(directory)], code=2)
        self.assertFalse(entered)
        self.assertEqual(result[0].state.value, "denied")
        self.assertNotIn("PRIVATE_SKILL_INSTRUCTIONS", json.dumps(result[0].model_dump(mode="json")))

    async def test_native_utf8_bom_and_crlf_keep_the_same_checked_content(self):
        for index, prefix, newline in [(0, b"", b"\n"), (1, b"\xef\xbb\xbf", b"\n"), (2, b"\xef\xbb\xbf", b"\r\n")]:
            directory = self.skill(f"encoding-{index}", "CHECKED_INSTRUCTIONS")
            path = directory / "SKILL.md"
            path.write_bytes(prefix + path.read_bytes().replace(b"\n", newline))
            captured, entered, result = await self.execute([str(directory)])
            self.assertEqual(captured[0]["aw_skill"]["directory"], str(directory))
            self.assertTrue(entered)
            self.assertIn("CHECKED_INSTRUCTIONS", json.dumps([item.model_dump(mode="json") for item in result]))

    async def test_loader_change_after_check_cannot_replace_the_pinned_object(self):
        first = self.skill("first", "CHECKED")
        second = self.skill("second", "UNCHECKED")
        async def switch(agent, payload):
            for group in agent.toolkit.tool_groups:
                group.skills_or_loaders = [LocalSkillLoader(str(second))]
        captured, entered, result = await self.execute([str(first)], before=switch)
        self.assertTrue(entered)
        self.assertEqual(captured[0]["aw_skill"]["directory"], str(first))
        text = json.dumps([item.model_dump(mode="json") for item in result])
        self.assertIn("CHECKED", text)
        self.assertNotIn("UNCHECKED", text)

    async def test_source_changes_during_check_block_delivery(self):
        directory = self.skill("first", "ORIGINAL")
        async def edit(agent, payload):
            (directory / "SKILL.md").write_text("changed")
        with self.assertLogs(PLUGIN.__name__, level="ERROR"):
            _, entered, result = await self.execute([str(directory)], before=edit)
        self.assertFalse(entered)
        self.assertEqual(result[0].state.value, "denied")

    async def test_source_changes_cannot_downgrade_a_policy_denial_to_report(self):
        directory = self.skill("first", "ORIGINAL")
        async def edit(agent, payload):
            (directory / "SKILL.md").write_text("changed")
        _, entered, result = await self.execute([str(directory)], code=2, before=edit, on_error="report")
        self.assertFalse(entered)
        self.assertEqual(result[0].state.value, "denied")

    async def test_memory_skill_content_must_match_its_actual_signed_source(self):
        directory = self.skill("first", "DISK")
        skill = Skill(name="example", description="fixture", dir=str(directory), markdown="DIFFERENT", updated_at=0)
        captured, _, _ = await self.execute([skill], code=2)
        self.assertEqual(captured[0]["aw_skill"], {"error_code":"skill_resolution_error"})

    async def test_missing_source_and_missing_skill_are_resolution_errors(self):
        directory = self.skill("first", "DISK")
        for skills, name in [([str(directory)], "absent"),
            ([Skill(name="example", description="fixture", dir=str(self.root / "absent"), markdown="X", updated_at=0)], "example")]:
            captured, entered, _ = await self.execute(skills, code=2, name=name)
            self.assertEqual(captured[0]["aw_skill"], {"error_code":"skill_resolution_error"})
            self.assertFalse(entered)

    async def test_unselected_skill_checks_preserve_native_memory_objects(self):
        skill = Skill(name="example", description="fixture", dir="/missing", markdown="MEMORY_ONLY", updated_at=0)
        captured, entered, result = await self.execute([skill], enabled=False)
        self.assertNotIn("aw_skill", captured[0])
        self.assertTrue(entered)
        self.assertIn("MEMORY_ONLY", json.dumps([item.model_dump(mode="json") for item in result]))

    async def test_cancellation_before_check_completion_never_enters_native_viewer(self):
        directory = self.skill("first", "CHECKED")
        started = asyncio.Event()
        async def held(agent, payload):
            started.set()
            await asyncio.Event().wait()
        task = asyncio.create_task(self.execute([str(directory)], before=held))
        await asyncio.wait_for(started.wait(), timeout=2)
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertEqual(sys.modules[SPEC.name + ".aw_skill"].metadata(), {})

    async def test_concurrent_native_calls_keep_their_own_skill_snapshots(self):
        first = self.skill("first", "ONE")
        second = self.skill("second", "TWO")
        results = await asyncio.gather(self.execute([str(first)]), self.execute([str(second)]))
        for result, expected in zip(results, [first, second]):
            self.assertEqual(result[0][0]["aw_skill"]["directory"], str(expected))


if __name__ == "__main__":
    unittest.main()
