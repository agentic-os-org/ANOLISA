"""Actual factory reply/reply_stream against a controlled OpenAI HTTP endpoint."""
import asyncio
import json
import os
import subprocess
from contextlib import aclosing
from pathlib import Path
from unittest.mock import patch

from agentscope.agent import InjectionConfig
from agentscope.credential import OpenAICredential
from agentscope.event import ToolResultTextDeltaEvent
from agentscope.message import AssistantMsg, Msg, TextBlock, UserMsg
from agentscope.middleware import MiddlewareBase
from agentscope.model import OpenAIChatModel
from agentscope.state import AgentState
from agentscope.permission import PermissionBehavior, PermissionDecision
from agentscope.tool import ToolBase, ToolChunk, Toolkit
from aw_agentscope import AwBridge
from aw_submission import SubmissionDenied
from aw_scope_ledger import AwTaskScope, CallbackFailure
import aw_scope_ledger as ledger

ROOT = Path(os.environ["AW_TEST_ROOT"])
AW = os.environ["AW_TEST_BIN"]
SOCKET = ROOT / "state/aw.sock"
REPORT = os.environ["AW_TEST_REPORT"] == "1"
SECURITY = os.environ["AW_TEST_SECURITY"] == "1"


def rows(name):
    path = ROOT / name
    return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []


def clean():
    result = json.loads(subprocess.check_output(
        [AW, "status", "--socket", str(SOCKET)], timeout=5
    ))["result"]
    # The aw run root binding lives until this worker exits.
    assert result["bindings"] == 1 and result["events"] == 0, result
    assert result["resources"]["usage"]["invocations"] == 0, result


class Original(MiddlewareBase):
    async def on_reply(self, agent, input_kwargs, next_handler):
        if isinstance(input_kwargs["inputs"], Msg) and input_kwargs["inputs"].get_text_content() == "rewrite":
            input_kwargs["inputs"] = UserMsg("user", "risk")
        async for item in next_handler(**input_kwargs):
            yield item


class LocalTool(ToolBase):
    name = "local_tool"
    description = "Record a local side effect"
    input_schema = {"type": "object", "properties": {"mode": {"type": "string"}}, "required": ["mode"]}
    is_read_only = False
    is_concurrency_safe = True

    async def check_permissions(self, tool_input, context):
        return PermissionDecision(message="fixture permission", behavior=PermissionBehavior.DENY if tool_input["mode"] == "permission" else PermissionBehavior.ALLOW)

    async def call(self, mode):
        with (ROOT / "tools.jsonl").open("a") as output:
            output.write(json.dumps({"mode": mode}) + "\n")
        if mode == "wait":
            try:
                yield ToolChunk(content=[TextBlock(text="partial")], is_last=False)
                await asyncio.sleep(30)
                raise AssertionError("tool survived stream close")
            finally:
                (ROOT / "tool-cleaning").touch()
                for _ in range(500):
                    if (ROOT / "tool-release").exists():
                        break
                    await asyncio.sleep(.01)
                assert (ROOT / "tool-release").exists(), "tool cleanup was not released"
                (ROOT / "tool-cancelled").touch()
        else:
            yield ToolChunk(content=[TextBlock(text="private-tool-result")], is_last=True)


async def main():
    bridge = AwBridge()
    models = []
    def make(session, stream=False, concurrent=True, context_size=128000):
        model = OpenAIChatModel(
            OpenAICredential(api_key="fixture", base_url=os.environ["AW_TEST_MODEL"]),
            model="fixture", stream=stream, max_retries=0, context_size=context_size,
        )
        models.append(model)
        tool = LocalTool()
        tool.is_concurrency_safe = concurrent
        return bridge.create_agent(
            name="fixture", system_prompt="fixture system", model=model,
            state=AgentState(session_id=session), middlewares=[Original()], toolkit=Toolkit(tools=[tool]),
            injection_config=InjectionConfig(inject_runtime_state=False),
        )
    agent = make("session-one")
    cases = []
    async def check(label, inputs, blocked=False, error=None, target=agent, stream=False):
        before = len(rows("models.jsonl"))
        side_effects = len(rows("tools.jsonl"))
        context = target.state.model_dump(mode="json")["context"]
        try:
            if stream:
                async with aclosing(target.reply_stream(inputs, yield_final_msg=True)) as response:
                    async for _ in response:
                        pass
            else:
                await target.reply(inputs)
        except (SubmissionDenied, ValueError) as exc:
            assert blocked or error is not None, (label, exc)
            assert error is None or isinstance(exc, error), (label, exc)
            assert len(rows("models.jsonl")) == before, label
            assert len(rows("tools.jsonl")) == side_effects, label
            assert target.state.model_dump(mode="json")["context"] == context, label
        else:
            assert not blocked and error is None, label
            assert len(rows("models.jsonl")) > before, label
        clean()
        cases.append(label)

    try:
        # Hold a real committed open response before the bridge sees its handle.
        entered, release = asyncio.Event(), asyncio.Event()
        original_command = AwTaskScope._command
        async def delayed_open(scope, action, *args, **kwargs):
            response = await original_command(scope, action, *args, **kwargs)
            if action == "open":
                entered.set()
                await release.wait()
            return response
        with patch.object(AwTaskScope, "_command", delayed_open):
            task = asyncio.create_task(agent.reply(UserMsg("user", "cancel-open")))
            try:
                await asyncio.wait_for(entered.wait(), 10)
                task.cancel()
                await asyncio.sleep(0)
                assert not task.done()
                release.set()
                task.cancel()
                try:
                    await task
                except asyncio.CancelledError:
                    pass
                else:
                    raise AssertionError("opening cancellation was lost")
            finally:
                release.set()
                if not task.done():
                    task.cancel()
                    await asyncio.gather(task, return_exceptions=True)
        assert not rows("models.jsonl") and not rows("tools.jsonl")
        clean()
        cases.append("cancel-open")
        for text in ["clean", "risk", "pii", "recovered", "error", "invalid", "timeout", "rewrite"]:
            await check(text, UserMsg("user", text), text in {"risk", "pii", "rewrite"} or (
                text in {"error", "invalid", "timeout"} and not REPORT
            ))
        # The controlled endpoint accepts this input without context compression.
        large = make("session-large", context_size=1000000)
        large_text = "x" * 1024 * 1024
        scans_before = len(rows("scans.jsonl")) + len(rows("scanner.jsonl"))
        await check("encoding-limit", UserMsg("user", large_text), not REPORT, target=large)
        assert len(rows("scans.jsonl")) + len(rows("scanner.jsonl")) == scans_before
        if REPORT:
            content = next(message["content"] for message in reversed(rows("models.jsonl")[-1]["messages"])
                           if message["role"] == "user")
            if isinstance(content, list):
                content = "".join(block.get("text", "") for block in content)
            assert content == large_text
        await check("encoding-limit-recover", UserMsg("user", "clean"), target=large)
        await check("string-rejected", "text", error=ValueError)
        await check("object-rejected", object(), error=ValueError)
        await check("media-rejected", UserMsg("user", [{"type": "data", "source": {
            "type": "url", "url": "https://example.invalid/image.png", "media_type": "image/png"}}]), error=ValueError)
        await check("blocks-list", [AssistantMsg("assistant", "history"), UserMsg("user", [
            TextBlock(text="first"), TextBlock(text="second")])])
        if SECURITY:
            assert rows("scanner.jsonl")[-1]["text"] == "first\nsecond"
        else:
            assert rows("scans.jsonl")[-1]["event"]["input"]["text"] == "first\nsecond"
        for mode in ["allow", "strict", "permission"]:
            count = len(rows("tools.jsonl"))
            await check("tool-" + mode, UserMsg("user", "tool-" + mode))
            assert len(rows("tools.jsonl")) - count == int(mode != "permission")
            if mode == "strict":
                latest = rows("models.jsonl")[-1]
                assert "result withheld by policy" in json.dumps(latest)
                # Prior allowed history may contain the secret; inspect the latest tool message.
                assert "private-tool-result" not in json.dumps(latest["messages"][-1])
        for concurrent in [True, False]:
            target = agent if concurrent else make("session-sequential", concurrent=False)
            label = "concurrent" if concurrent else "sequential"
            for name in ["tool-cleaning", "tool-cancelled", "tool-release"]:
                (ROOT / name).unlink(missing_ok=True)
            async def verify_teardown():
                while not (ROOT / "tool-cleaning").exists():
                    await asyncio.sleep(.001)
                try:
                    assert not (ROOT / "tool-cancelled").exists()
                    try:
                        await target.reply(UserMsg("user", "during-cleanup"))
                    except CallbackFailure:
                        pass
                    else:
                        raise AssertionError("reply admitted during tool teardown")
                    status = json.loads(subprocess.check_output(
                        [AW, "status", "--socket", str(SOCKET)], timeout=5
                    ))["result"]
                    assert status["bindings"] == 2, status
                finally:
                    (ROOT / "tool-release").touch()
            watcher = asyncio.create_task(verify_teardown())
            try:
                async with aclosing(target.reply_stream(UserMsg("user", "tool-wait"))) as response:
                    async for event in response:
                        if isinstance(event, ToolResultTextDeltaEvent):
                            break
                await asyncio.wait_for(watcher, 5)
            finally:
                if not watcher.done():
                    watcher.cancel()
                    await asyncio.gather(watcher, return_exceptions=True)
            assert (ROOT / "tool-cancelled").exists()
            clean()
            cases.append(label + "-tool-stream-close")
            await check(label + "-tool-stream-recover", UserMsg("user", "clean"), target=target)
        scans = len(rows("scanner.jsonl")) if SECURITY else sum(row["event"]["name"] == "input.submit" for row in rows("scans.jsonl"))
        await check("resume", None)
        await check("history", AssistantMsg("assistant", "risk"))
        assert (len(rows("scanner.jsonl")) if SECURITY else sum(row["event"]["name"] == "input.submit" for row in rows("scans.jsonl"))) == scans
        streamed = make("session-stream", True)
        await check("stream-allow", UserMsg("user", "clean"), target=streamed, stream=True)
        await check("stream-deny", UserMsg("user", "risk"), True, target=streamed, stream=True)
        # Close while suspended at a native stream event, before full consumption.
        async with aclosing(streamed.reply_stream(UserMsg("user", "early"))) as response:
            await anext(response)
        clean()
        cases.append("stream-close")
        await check("stream-recover", UserMsg("user", "recovered"), target=streamed, stream=True)
        # Make the actual close command fail to connect, retaining the live scope.
        failed_closes = []
        async def unavailable_close(scope, action, *args, **kwargs):
            if action != "close":
                return await original_command(scope, action, *args, **kwargs)
            displaced = ROOT / "withheld.sock"
            SOCKET.rename(displaced)
            try:
                response = await original_command(scope, action, *args, **kwargs)
            finally:
                displaced.rename(SOCKET)
            assert response[0] != 0, response
            failed_closes.append(scope)
            return response
        with patch.object(AwTaskScope, "_command", unavailable_close):
            response = agent.reply_stream(UserMsg("user", "close-failure"))
            await anext(response)
            try:
                await response.aclose()
            except CallbackFailure as error:
                assert str(error) == "aw_scope_cleanup_failed", error
            else:
                raise AssertionError("explicit stream close hid cleanup failure")
            scope = failed_closes[-1]
            assert not scope.closed and scope in ledger._unclosed_scopes
            calls = len(rows("models.jsonl"))
            try:
                await agent.reply(UserMsg("user", "blocked-by-pending-cleanup"))
            except CallbackFailure as error:
                assert str(error) == "aw_scope_cleanup_failed", error
            else:
                raise AssertionError("new reply admitted with pending cleanup")
            assert len(rows("models.jsonl")) == calls
        cases.append("scope-close-failure")
        await check("scope-close-recover", UserMsg("user", "clean"))
        assert scope.closed and scope not in ledger._unclosed_scopes
        # Cancellation during Provider invocation, with a second cancellation in cleanup.
        entered = ROOT / "scan-entered"
        entered.unlink(missing_ok=True)
        calls = len(rows("models.jsonl"))
        task = asyncio.create_task(agent.reply(UserMsg("user", "cancel")))
        for _ in range(500):
            if entered.exists():
                break
            await asyncio.sleep(.01)
        assert entered.exists()
        try:
            await agent.reply(UserMsg("user", "concurrent"))
        except CallbackFailure:
            pass
        else:
            raise AssertionError("same-agent concurrent reply admitted")
        task.cancel()
        await asyncio.sleep(.01)
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass
        assert len(rows("models.jsonl")) == calls
        clean()
        cases.append("cancel-and-concurrent")
        await check("cancel-recover", UserMsg("user", "recovered"))
        other = make("session-two")
        await asyncio.gather(agent.reply(UserMsg("user", "one")), other.reply(UserMsg("user", "two")))
        clean()
        cases.append("parallel-agents")
        scans = [row for row in rows("scans.jsonl") if row["event"]["name"] == "input.submit"]
        grouped = {}
        for row in scans:
            event = row["event"]
            assert "tool" not in event
            grouped.setdefault(event["turn_id"], []).append(event)
        for group in grouped.values():
            assert all(event == group[0] for event in group)
            text = group[0]["input"]["text"]
            expected = 1 if text in {"risk", "cancel"} or (not REPORT and text in {"error", "invalid", "timeout"}) else 2
            assert len(group) == expected, (text, len(group), expected)
        assert len({group[0]["agent"]["instance_id"] for group in grouped.values()}) == len(grouped)
        if SECURITY:
            for row in rows("scanner.jsonl"):
                assert row["context"]["agent_name"] == "agentscope"
                assert row["context"]["session_id"] and row["context"]["run_id"]
            assert any(row["prompt"] and row["text"] == "risk" for row in rows("scanner.jsonl"))
            assert any(not row["prompt"] and row["text"] == "pii" for row in rows("scanner.jsonl"))
        print(json.dumps({"cases": cases, "turns": len(grouped)}))
    finally:
        for model in models:
            await model.client.close()


asyncio.run(main())
