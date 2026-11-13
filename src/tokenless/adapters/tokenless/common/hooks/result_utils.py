"""Host-independent values and transformations used at the hook boundary."""

from __future__ import annotations

import json
from dataclasses import dataclass

from hook_utils import (
    MIN_SEARCH_LISTING_CHARS,
    SHELL_TOOLS,
    SEARCH_TOOLS,
    SKIP_TOOLS,
    skip,
    try_parse_json,
)


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
    search_listing: bool = False

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


def is_search_tool(tool_name: str) -> bool:
    """Whether the tool is a native search alias whose text is match listings.

    Search listings are derived match data, not authoritative file copies:
    their repeated paths can be folded losslessly while every received match,
    line number, order, duplicate and line ending is retained. File reads keep
    their ``file_content`` origin and stay byte-for-byte.
    """
    return tool_name in SEARCH_TOOLS


def search_text_field(
    tool_name: str, envelope: object, slots: tuple[str, ...]
) -> tuple[str, str] | None:
    """The search tool's text slot inside its result envelope, or ``None``.

    A native search result keeps its host envelope (``mode`` counters,
    truncation indicators, other metadata): only the named slot holds the
    match listing the lossless search compressor reads. Among several
    eligible slots the largest one wins — the same rule
    :func:`select_text_field` applies to shell envelopes — so a secondary
    short slot never masks the real listing. A slot is eligible when it is
    a string at or above the shared minimum so tiny or empty results never
    become the compression target.
    """
    if not is_search_tool(tool_name) or not isinstance(envelope, dict):
        return None
    return select_text_field(envelope, slots, MIN_SEARCH_LISTING_CHARS)


def search_origin(
    tool_name: str, envelope: object, slots: tuple[str, ...] = ("content", "output", "text")
) -> str | None:
    """The origin to declare for a native search result, or ``None``.

    An envelope that carries the match listing in one text slot (or that is
    the listing itself) declares ``api_response`` — Core routes that derived
    match text through the lossless search-only path, and replacement
    rewrites only the received slot. Any other shape keeps the caller's
    classification: unknown search envelopes are preserved, never guessed.

    The verdict is derived from :func:`search_text_field` so the declared
    origin and the slot replacement rewrites cannot drift apart.
    """
    if not is_search_tool(tool_name):
        return None
    if isinstance(envelope, str):
        return "api_response" if len(envelope) >= MIN_SEARCH_LISTING_CHARS else None
    if isinstance(envelope, dict):
        if search_text_field(tool_name, envelope, slots) is not None:
            return "api_response"
    return None


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
