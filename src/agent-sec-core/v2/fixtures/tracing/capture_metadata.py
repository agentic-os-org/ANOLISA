"""Capture the V1 metadata oracle; run with the V1 development environment."""

import json
from pathlib import Path

from agent_sec_cli.observability.schema import (
    ModelCallMetadata,
    ObservabilityMetadata,
    ToolCallMetadata,
)
from pydantic import ValidationError


def main() -> None:
    base = {
        "sessionId": "session",
        "runId": "run",
        "callId": "call",
        "toolCallId": "tool",
    }
    inputs = [
        base,
        None,
        [],
        "invalid",
        {},
        {"session_id": "s", "run_id": "r", "tool_call_id": "t"},
    ]
    for field in base:
        inputs.append({key: value for key, value in base.items() if key != field})
        for value in [None, 42, False, [], {}, "", " \t ", "🦀" * 300]:
            inputs.append(base | {field: value})
    inputs.extend(
        [
            base | {"session_id": "snake", "run_id": "snake", "tool_call_id": "snake"},
            base | {"sessionId": None, "session_id": "must-not-fallback"},
            base | {"callId": None, "call_id": "must-not-fallback"},
            base | {"agentName": 123, "unknown": {"secret": "ignored"}},
        ]
    )
    cases = []
    for value in inputs:
        expected = {}
        for kind, schema in [
            ("agent", ObservabilityMetadata),
            ("model", ModelCallMetadata),
            ("tool", ToolCallMetadata),
        ]:
            try:
                expected[kind] = schema.model_validate(value).model_dump(
                    exclude_none=True
                )
            except ValidationError:
                expected[kind] = None
        cases.append({"input": value, "expected": expected})
    Path(__file__).with_name("metadata.json").write_text(
        json.dumps(cases, ensure_ascii=False, indent=2) + "\n"
    )


if __name__ == "__main__":
    main()
