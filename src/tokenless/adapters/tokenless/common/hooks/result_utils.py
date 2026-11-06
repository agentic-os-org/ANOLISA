"""Host-independent values and transformations used at the hook boundary."""

from __future__ import annotations

import json
from dataclasses import dataclass

from hook_utils import SHELL_TOOLS, SKIP_TOOLS, skip, try_parse_json


@dataclass
class ToolResult:
    """Normalized model content and the capabilities of its replacement slot."""

    value: object
    envelope: object
    field: tuple[str, str] | None
    origin: str
    command: str | None
    status: str
    replace_output: bool
    replace_with_text: bool

    def content(self) -> str:
        value = self.field[1] if self.field is not None else self.value
        if isinstance(value, str):
            return value
        if isinstance(value, (dict, list)):
            return json.dumps(value, separators=(",", ":"), ensure_ascii=False)
        skip()

    def replacement(self, output: str) -> object:
        if self.field is not None:
            return {**self.envelope, self.field[0]: output}
        if self.replace_with_text:
            return output
        return try_parse_json(output)


def select_text_field(
    envelope: object, fields: tuple[str, ...], minimum: int
) -> tuple[str, str] | None:
    """Select the largest eligible text slot without changing other fields."""
    if not isinstance(envelope, dict):
        return None
    selected = None
    for name in fields:
        value = envelope.get(name)
        if isinstance(value, str) and len(value) >= minimum:
            if selected is None or len(value) > len(selected[1]):
                selected = (name, value)
    return selected


def content_origin(tool_name: str) -> str:
    if tool_name in SKIP_TOOLS:
        return "file_content"
    if tool_name in SHELL_TOOLS:
        return "command_output"
    return "api_response"


def process_result(value: object) -> dict | None:
    """Decode a structured process result, including double-encoded JSON."""
    for _ in range(2):
        if isinstance(value, str):
            value = try_parse_json(value)
    return value if isinstance(value, dict) else None


def result_status(status: str, is_error: bool, process: dict | None) -> str:
    status = str(status).lower()
    if status in {"interrupted", "denied"}:
        return status
    if is_error or (
        process is not None
        and (
            process.get("error") is not None
            or process.get("exit_code") not in (None, 0)
            or process.get("exitCode") not in (None, 0)
        )
    ):
        return "error"
    return "success"


def error_content(result: ToolResult) -> str:
    """Use an error stream for diagnosis while retaining the original output."""
    process = (
        process_result(result.value) if result.origin == "command_output" else None
    )
    if result.status == "error" and process is not None:
        parts = [process.get(field) for field in ("stderr", "error")]
        parts = [part for part in parts if isinstance(part, str) and part.strip()]
        if parts:
            return "\n".join(parts)
    return result.content()
