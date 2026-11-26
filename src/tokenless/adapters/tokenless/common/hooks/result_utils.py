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
    exit_code: int | None = None
    text_slot: bool = False

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


def extract_exit_code(envelope: object, data: dict | None = None) -> int | None:
    """Return an integer process exit code from a host envelope, nested process, or payload."""
    sources: list[object] = [envelope]
    if isinstance(envelope, dict):
        sources.append(envelope.get("process"))
    sources.append(data)
    for source in sources:
        if not isinstance(source, dict):
            continue
        for key in ("exit_code", "exitCode", "returncode", "return_code"):
            value = source.get(key)
            if isinstance(value, int) and not isinstance(value, bool):
                return value
    return None


_PROCESS_ENVELOPE_KEYS = frozenset(
    {"stdout", "stderr", "exit_code", "exitCode", "returncode", "return_code"}
)


def is_process_envelope(value: object) -> bool:
    """Return True when a decoded dict looks like a host shell process envelope."""
    return isinstance(value, dict) and (
        bool(value.keys() & _PROCESS_ENVELOPE_KEYS)
        or isinstance(value.get("process"), dict)
    )


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


def post_tool_payload_fields(
    result: ToolResult, data: dict | None = None
) -> tuple[str, int | None, bool]:
    """Return ``(content, exit_code, replace_output)`` keeping stream slots aligned."""
    process = (
        None
        if result.text_slot
        else (
            process_result(result.value) if result.origin == "command_output" else None
        )
    )
    host_envelope = process if is_process_envelope(process) else None
    exit_code = result.exit_code
    if exit_code is None:
        exit_code = extract_exit_code(
            host_envelope if host_envelope is not None else result.envelope, data
        )

    if result.field is not None and (
        result.replace_output or result.status != "error"
    ):
        return result.field[1], exit_code, result.replace_output

    if result.status == "error":
        if (
            host_envelope is None
            and isinstance(result.value, str)
            and result.value
            and result.replace_output
            and exit_code is not None
        ):
            return result.value, exit_code, True
        return error_content(result), exit_code, False

    return result.content(), exit_code, result.replace_output
