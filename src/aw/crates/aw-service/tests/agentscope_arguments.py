"""Compare AW inputs with the pinned SDK's permission and executed arguments."""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

from agentscope_cancellation import owned

EXACT_ARGUMENTS = {
    "command": "echo json-domain",
    "options": {
        "values": [
            None,
            True,
            False,
            0,
            1,
            -0.0,
            0.0,
            1.0,
            0.1,
            5e-324,
            sys.float_info.max,
            '中😀\n\t\0"\\',
            {"键": [[], {}, "值😀"]},
        ]
    },
}

CASES = [
    (json.dumps(EXACT_ARGUMENTS), EXACT_ARGUMENTS, "success"),
    ('{"command":"echo allowed"', {"command": "echo allowed"}, "success"),
    ('{"command":"echo recursive"}', {"command": "echo recursive"}, "recursive"),
    ('{"command":"echo integer-limit"}', {"command": "echo integer-limit"}, "integer_limit"),
    *[
        (
            json.dumps({"command": "echo numeric", "options": {"values": [value]}}),
            {"command": "echo numeric", "options": {"values": [value]}},
            state,
        )
        for value, state in [
            (-(2**63), "success"),
            (2**64 - 1, "success"),
            (-(2**63) - 1, "out_of_range"),
            (2**64 + 1, "out_of_range"),
        ]
    ],
    ("{'command':'echo single'}", {"command": "echo single"}, "success"),
    (
        '```json\n{"command":"echo 你好","options":{"nested":[true,null]}}\n```',
        {"command": "echo 你好", "options": {"nested": [True, None]}},
        "success",
    ),
    ('{"command":"echo \\ud83d\\ude00"}', {"command": "echo 😀"}, "success"),
    ('{"command":"echo \\ud800"}', {"command": "echo \ud800"}, "unencodable"),
    ('{"command":"echo \\udfff"}', {"command": "echo \udfff"}, "unencodable"),
    (
        '{"command":"echo literal-surrogates"}',
        {"command": "echo literal-surrogates"},
        "literal_surrogates",
    ),
    ('{"command":"echo denied"', {"command": "echo denied"}, "denied"),
]


def exact_json(value: Any) -> str:
    # Python equality hides bool/int and signed-zero differences across the wire.
    return json.dumps(value, sort_keys=True, allow_nan=False)


def record(name: str, value: Any) -> None:
    with (Path(os.environ["AW_TEST_ROOT"]) / (name + ".jsonl")).open("a") as output:
        output.write(json.dumps(value) + "\n")


async def worker() -> None:
    from agentscope.agent import InjectionConfig
    from agentscope.message import TextBlock, ToolCallBlock, UserMsg
    from agentscope.middleware import MiddlewareBase
    from agentscope.permission import PermissionBehavior, PermissionDecision
    from agentscope.state import AgentState
    from agentscope.tool import ToolBase, ToolChunk, Toolkit
    from aw_agentscope import AwBridge
    from fixtures.agentscope.worker import OfflineModel

    class Tool(ToolBase):
        name = "exec"
        description = "Record arguments without executing shell commands"
        input_schema = {
            "type": "object",
            "properties": {"command": {"type": "string"}, "options": {"type": "object"}},
            "required": ["command"],
        }
        is_read_only = False
        is_concurrency_safe = False

        async def check_permissions(self, tool_input: dict[str, Any], context: Any) -> Any:
            record("permissions", tool_input)
            return PermissionDecision(behavior=PermissionBehavior.ALLOW, message="Fixture allow")

        async def call(self, command: str, **options: Any) -> Any:
            record("executed", {"command": command, **options})
            if command == "echo \ud83d\ude00":
                record("surrogate_codepoints", [ord(c) for c in command[-2:]])
            yield ToolChunk(content=[TextBlock(text="done")], is_last=True)

    class Rewrite(MiddlewareBase):
        async def on_acting(self, agent: Any, input_kwargs: Any, next_handler: Any) -> Any:
            call = input_kwargs["tool_call"]
            if call.input == '{"command":"echo recursive"}':
                call = call.model_copy(
                    update={"input": '{"options":' + "[" * 10000 + "0" + "]" * 10000 + "}"}
                )
            elif call.input == '{"command":"echo integer-limit"}':
                call = call.model_copy(
                    update={
                        "input": '{"command":"echo integer-limit","options":{"value":'
                        + "9" * 10000
                        + "}}"
                    }
                )
            elif call.input == '{"command":"echo literal-surrogates"}':
                call = call.model_copy(update={"input": '{"command":"echo \ud83d\ude00"}'})
            async for item in next_handler(tool_call=call):
                yield item

    model = OfflineModel([])
    agent = AwBridge().create_agent(
        name="arguments",
        system_prompt="Run local fixture tools",
        model=model,
        toolkit=Toolkit(tools=[Tool()]),
        state=AgentState(session_id="arguments-session"),
        injection_config=InjectionConfig(inject_runtime_state=False),
        middlewares=[Rewrite()],
    )
    for raw, expected, state in CASES:
        if state in {"recursive", "integer_limit"}:
            state = "denied" if os.environ["AW_TEST_ACTION"] == "block" else "error"
        if state in {"unencodable", "out_of_range", "literal_surrogates"}:
            state = "denied" if os.environ["AW_TEST_ACTION"] == "block" else "success"
        model.calls = [ToolCallBlock(id="reused", name="exec", input=raw)]
        model.round = 0
        await asyncio.wait_for(agent.reply(UserMsg("user", "run")), 10)
        results = [
            b
            for msg in agent.state.model_dump(mode="json")["context"]
            for b in msg["content"]
            if b["type"] == "tool_result"
        ]
        assert results[-1]["state"] == state, (raw, expected, results[-1])


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--aw", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--on-error", choices=["report", "block"])
    args = parser.parse_args()
    aw = str(args.aw.resolve())
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    script = str(Path(__file__).resolve())
    for action in [args.on_error] if args.on_error else ["report", "block"]:
        with tempfile.TemporaryDirectory(prefix="s-", dir=output) as temporary:
            root = Path(temporary)
            state = root / "state"
            assert len(os.fsencode(state / "aw.sock")) <= 107
            config = {
                "apiVersion": "aw/v1alpha1",
                "kind": "AWConfiguration",
                "metadata": {"name": "arguments"},
                "spec": {
                    "daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(state)},
                    "execution": {"guarantee": "native_hook", "default_event_budget_ms": 10000},
                    "audit": {"enabled": True, "payload": "metadata_only"},
                    "agents": {
                        "sdk": {
                            "adapter": "agentscope",
                            "argv": [sys.executable, script, "--worker"],
                        }
                    },
                    "providers": {},
                    "events": {},
                },
            }
            for phase in ["before", "after"]:
                config["spec"]["providers"][phase] = {
                    "transport": {
                        "type": "stdio",
                        "location": "agent",
                        "argv": [sys.executable, script, "--policy", phase],
                    },
                    "timeout_ms": 2000,
                    "max_output_bytes": 32,
                }
                config["spec"]["events"]["tool." + phase] = {
                    "enabled": True,
                    "steps": [
                        {
                            "id": phase,
                            "provider": phase,
                            "effects": ["observe", "block"] if phase == "before" else ["observe"],
                            "on_error": action if phase == "before" else "report",
                        }
                    ],
                }
            path = root / "aw.json"
            path.write_text(json.dumps(config))
            env = {**os.environ, "AW_TEST_ROOT": str(root), "AW_TEST_ACTION": action}
            with owned(
                [aw, "serve", "--config", str(path), "--state-dir", str(state)],
                root,
                output,
                "serve-" + action,
                env,
            ) as service:
                try:
                    deadline = time.monotonic() + 5
                    while not (state / "aw.sock").exists():
                        assert (
                            service.poll() is None and time.monotonic() < deadline
                        ), "startup failed"
                        time.sleep(0.01)
                    with owned(
                        [aw, "run", "--config", str(path), "--agent", "sdk"],
                        root,
                        output,
                        "worker-" + action,
                        env,
                    ) as child:
                        child.wait(timeout=50)
                        assert child.returncode == 0, (
                            output / ("worker-" + action + ".stderr")
                        ).read_text()

                    def records(name: str) -> list[Any]:
                        return [
                            json.loads(line)
                            for line in (root / (name + ".jsonl")).read_text().splitlines()
                        ]

                    expected = [case[1] for case in CASES]
                    assert exact_json(records("permissions")) == exact_json(expected)
                    executed = [
                        ({"command": "echo 😀"} if state == "literal_surrogates" else value)
                        for _, value, state in CASES
                        if state == "success"
                        or (
                            state in {"unencodable", "out_of_range", "literal_surrogates"}
                            and action == "report"
                        )
                    ]
                    assert exact_json(records("executed")) == exact_json(executed)
                    if action == "report":
                        assert records("surrogate_codepoints") == [[0xD83D, 0xDE00]]
                    else:
                        assert not (root / "surrogate_codepoints.jsonl").exists()
                    for phase in ["before", "after"]:
                        events = records(phase)
                        accepted = [
                            value
                            for _, value, state in CASES
                            if state
                            not in {
                                "unencodable",
                                "recursive",
                                "integer_limit",
                                "out_of_range",
                                "literal_surrogates",
                            }
                        ]
                        assert exact_json([e["tool"]["input"] for e in events]) == exact_json(
                            accepted
                        )
                        assert [e["native"]["tool_call"]["input"] for e in events] == [
                            raw
                            for raw, _, state in CASES
                            if state
                            not in {
                                "unencodable",
                                "recursive",
                                "integer_limit",
                                "out_of_range",
                                "literal_surrogates",
                            }
                        ]
                    status = json.loads(
                        subprocess.check_output(
                            [aw, "status", "--socket", str(state / "aw.sock")], timeout=3
                        )
                    )["result"]
                    assert status["bindings"] == status["events"] == 0, status
                    diagnostics = (output / ("worker-" + action + ".stderr")).read_text()
                    assert "UnicodeEncodeError" not in diagnostics, diagnostics
                    assert "native_input_invalid_unicode" in diagnostics, diagnostics
                    assert "native_arguments_invalid" in diagnostics, diagnostics
                    assert "native_integer_out_of_range" in diagnostics, diagnostics
                    (output / ("result-" + action + ".json")).write_text(
                        json.dumps(
                            {"cases": len(records("permissions")), "status": status}, indent=2
                        )
                    )
                finally:
                    (output / ("records-" + action + ".json")).write_text(
                        json.dumps({p.name: p.read_text() for p in root.glob("*.jsonl")}, indent=2)
                    )
                    subprocess.run(
                        [aw, "stop", "--socket", str(state / "aw.sock")],
                        timeout=5,
                        capture_output=True,
                    )
                    service.wait(timeout=5)
    print(
        "Real SDK arguments: repair/Unicode inputs and report/block decoder-limit failures passed"
    )


if __name__ == "__main__":
    if sys.argv[1:2] == ["--worker"]:
        asyncio.run(worker())
    elif sys.argv[1:2] == ["--policy"]:
        event = json.load(sys.stdin)
        record(sys.argv[2], event)
        print(
            json.dumps(sys.argv[2] == "after" or event["tool"]["input"]["command"] == "echo denied")
        )
    else:
        main()
