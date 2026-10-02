#!/usr/bin/env python3
"""Exercise installed wheels with a real AgentScope model and static retrieval tool."""

import asyncio
import hashlib
import json
import os
import re
import sqlite3
import subprocess
import sys
from dataclasses import asdict
from pathlib import Path
from typing import ClassVar

RESULTS = Path("/results")
MANIFEST = json.loads(Path("/inputs/manifest.json").read_text())
TOOL = "tenant_tokenless_retrieve"
REFERENCE = re.compile(
    rf"If needed, call tool {TOOL} with hash_or_marker=([0-9a-f]{{24}})(?![\w-])"
)
REPORT = {"checks": [], "live": []}
SECRET = ""


def save(name: str, value: object) -> None:
    text = json.dumps(value, ensure_ascii=False, indent=2) + "\n"
    (RESULTS / name).write_text(text.replace(SECRET, "[REDACTED]") if SECRET else text)


def rows(directory: Path, database: str, query: str) -> list[dict]:
    with sqlite3.connect(f"file:{directory / database}?mode=ro", uri=True) as connection:
        connection.row_factory = sqlite3.Row
        return [dict(row) for row in connection.execute(query)]


async def exercise() -> None:
    import agentscope
    from agentscope.agent import Agent
    from agentscope.agent._config import ReActConfig
    from agentscope.credential import OpenAICredential
    from agentscope.message import Msg, TextBlock
    from agentscope.model import OpenAIChatModel
    from agentscope.permission import PermissionBehavior, PermissionDecision
    from agentscope.tool import ToolBase, ToolChunk, Toolkit
    from anolisa_tokenless import (
        Attribution,
        ContentOrigin,
        OutputOptimization,
        PostToolCapabilities,
        PostToolRequest,
        RecoveryMethod,
        ResultKind,
        TokenlessConfig,
        TokenlessSdk,
        ToolResultStatus,
    )
    from tokenless_agentscope import TokenlessAgentScope, ToolContract

    records = json.loads(Path("/inputs/records.json").read_text())
    raw = json.dumps(records, separators=(",", ":"))
    sdk = TokenlessSdk(TokenlessConfig(data_dir=RESULTS / "preflight", rtk_enabled=False))
    result = await sdk.post_tool(
        PostToolRequest(
            result_kind=ResultKind.TOOL,
            tool_name="records_fixture",
            content=raw,
            status=ToolResultStatus.SUCCESS,
            content_origin=ContentOrigin.API_RESPONSE,
            output_optimization=OutputOptimization.NONE,
            capabilities=PostToolCapabilities(True, RecoveryMethod.tool(TOOL), True),
            attribution=Attribution("release-regression", "preflight", "fixture"),
        )
    )
    assert REFERENCE.search(result.output) and "<<tokenless:" not in result.output
    target = next(
        record["id"]
        for record in records[4:-4]
        if record["status"] == "ok" and f'request-{record["id"]} ' not in result.output
    )
    REPORT["checks"].append(
        {"case": "installed_static_tool_candidate", "status": "passed", "target": target}
    )
    REPORT["installation"] = {"agent_version": agentscope.__version__, "wheels": MANIFEST["wheels"]}
    if not MANIFEST["live_requested"]:
        REPORT["live"].append({"status": "not_run", "reason": "no API key file supplied"})
        return

    directory = RESULTS / "live"
    model_calls = []
    fixture_calls = []

    class RecordsTool(ToolBase):
        """Synthetic data, deliberately separate from the real build workload."""

        name = "records_fixture"
        description = "Read the synthetic records fixture once."
        input_schema: ClassVar[dict] = {
            "type": "object",
            "properties": {},
            "additionalProperties": False,
        }
        is_concurrency_safe = True
        is_read_only = True

        async def check_permissions(self, tool_input: dict, context: object) -> PermissionDecision:
            return PermissionDecision(
                behavior=PermissionBehavior.ALLOW, message="Read-only fixture."
            )

        async def call(self) -> ToolChunk:
            fixture_calls.append("called")
            return ToolChunk(content=[TextBlock(text=raw)])

    class CaptureModel(OpenAIChatModel):
        """Record the actual boundary without altering model inputs or outputs."""

        async def __call__(
            self, messages: list, tools: list | None = None, **kwargs: object
        ) -> object:
            call = {
                "messages": [message.model_dump(mode="json") for message in messages],
                "tools": tools,
            }
            model_calls.append(call)
            save("model-calls.json", model_calls)
            response = await super().__call__(messages=messages, tools=tools, **kwargs)
            call["response"] = {
                "content": [block.model_dump(mode="json") for block in response.content],
                "usage": asdict(response.usage) if response.usage else None,
                "finished_reason": response.finished_reason,
            }
            save("model-calls.json", model_calls)
            return response

    integration = TokenlessAgentScope(
        TokenlessConfig(data_dir=directory, retrieve_tool_name=TOOL, rtk_enabled=False),
        tool_contracts={"records_fixture": ToolContract(ContentOrigin.API_RESPONSE)},
    )
    model = CaptureModel(
        credential=OpenAICredential(
            api_key=SECRET,
            base_url="https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
        ),
        model=MANIFEST["model"],
        stream=False,
        max_retries=0,
    )
    agent = Agent(
        name="release-regression",
        system_prompt="Use the provided tools. Never invent records.",
        model=model,
        toolkit=Toolkit(tools=[RecordsTool(), *integration.tools]),
        middlewares=integration.middlewares,
        react_config=ReActConfig(max_iters=6),
    )
    prompt = (
        "Call records_fixture exactly once. This is a synthetic recovery contract, not real workload data. "
        f"Follow its recovery instruction exactly once to recover the full array, then report the message for id {target}. "
        "Do not invent a shell command or guess the missing message."
    )
    final = await agent.reply(Msg(name="user", role="user", content=[TextBlock(text=prompt)]))
    save("final.json", final.model_dump(mode="json"))
    assert len(fixture_calls) == 1
    tools = [call["tools"] for call in model_calls]
    assert all(value == tools[0] for value in tools), "Tool definitions changed across model calls"
    assert {tool["function"]["name"] for tool in tools[0]} == {"records_fixture", TOOL}
    calls = [
        block
        for call in model_calls
        for block in call["response"]["content"]
        if block["type"] == "tool_call"
    ]
    retrieve_calls = [call for call in calls if call["name"] == TOOL]
    assert len(retrieve_calls) == 1, retrieve_calls
    stats = rows(directory, "stats.db", "SELECT * FROM stats")
    applied = [
        row
        for row in stats
        if "json_record_reduction" in json.loads(row["applied_operations"] or "[]")
    ]
    assert len(applied) == 1 and applied[0]["stash_writes"] == 1
    stashed = rows(directory, "stash.db", "SELECT hash, payload FROM stash")
    assert len(stashed) == 1
    entry = stashed[0]
    assert entry["payload"] == raw
    hits = rows(directory, "stats.db", "SELECT * FROM retrieve_events")
    assert len(hits) == 1 and hits[0]["outcome"] == "hit" and hits[0]["source"] == "embedded"
    assert (
        hits[0]["agent_id"] == "release-regression"
        and hits[0]["session_id"] == agent.state.session_id
    )
    visible_markers = agent.state.middle_context["anolisa_tokenless"]["visible_markers"]
    assert (
        entry["hash"] in visible_markers
    ), "BeforeModel did not authorize the model-visible reference"
    tool_results = [
        block
        for call in model_calls
        for message in call["messages"]
        for block in message["content"]
        if block["type"] == "tool_result"
    ]
    visible = [block for block in tool_results if block["id"] == applied[0]["tool_use_id"]]
    restored = [block for block in tool_results if block["id"] == retrieve_calls[0]["id"]]
    assert visible and REFERENCE.search(json.dumps(visible))
    assert (
        restored and restored[-1]["output"][0]["text"] == raw
    ), "Recovery was not delivered byte-for-byte"
    assert not any(
        row["tool_use_id"] == retrieve_calls[0]["id"] for row in stats
    ), "Retrieve output was compressed again"
    assert f"request-{target}" in json.dumps(final.model_dump())
    gross = applied[0]["before_tokens"] - applied[0]["after_tokens"]
    REPORT["live"].append(
        {
            "case": "static_tool_records",
            "status": "passed",
            "retrieve_source": "embedded",
            "gross_saved_tokens": gross,
            "retrieved_tokens": hits[0]["payload_tokens"],
            "saved_minus_retrieved_tokens": gross - hits[0]["payload_tokens"],
            "model_tool_payload_byte_exact": True,
            "visible_markers": visible_markers,
            "model_calls": len(model_calls),
            "tool_calls": calls,
        }
    )


def main() -> None:
    global SECRET
    if "--installed" not in sys.argv:
        environment = RESULTS / "venv"
        subprocess.run(
            [sys.executable, "-m", "venv", "--system-site-packages", str(environment)], check=True
        )
        packages = []
        for wheel in MANIFEST["wheels"].values():
            path = Path("/inputs") / wheel["file"]
            assert hashlib.sha256(path.read_bytes()).hexdigest() == wheel["sha256"]
            packages.append(str(path))
        python = str(environment / "bin/python")
        subprocess.run(
            [python, "-m", "pip", "install", "--no-index", "--no-deps", *packages], check=True
        )
        os.execv(python, [python, __file__, "--installed"])
    if MANIFEST["live_requested"]:
        SECRET = Path("/run/tokenplan-key").read_text().strip()
        assert SECRET, "API key file is empty"
    os.environ.update({"TOKENLESS_STATS_ENABLED": "1", "TOKENLESS_SLS_ENABLED": "0"})
    asyncio.run(exercise())


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        REPORT["error"] = str(error)
        import traceback

        save("failure.json", traceback.format_exc())
    finally:
        save("report.json", REPORT)
    raise SystemExit(int("error" in REPORT))
