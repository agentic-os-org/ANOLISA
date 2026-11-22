"""Capabilities and route confirmation helpers for aw_sdk.sources."""

from __future__ import annotations

from typing import Any, Iterable, Mapping

from aw_sdk.transport import AwError

SOURCE_PROTOCOL = "aw-hook-source/v1"
DEFAULT_MAX_STREAM_BYTES = 1024 * 1024
SUPPORTED_EVENTS: dict[str, tuple[str, ...]] = {
    "session.start": ("observe",),
    "turn.stop": ("observe", "block"),
    "session.end": ("observe",),
    "input.submit": ("observe", "block"),
    "tool.before": ("observe", "block"),
    "tool.after": ("observe",),
}
DEFAULT_EVENTS: tuple[str, ...] = ("input.submit", "tool.before", "tool.after")


class SourceDenied(AwError):
    """Raised when AW returns a block decision before native work starts."""

    def __init__(
        self,
        event: str,
        *,
        session_id: str,
        call_id: str | None = None,
        turn_id: str | None = None,
        tool_name: str | None = None,
    ) -> None:
        super().__init__("source_denied", unavailable=False)
        self.event = event
        self.session_id = session_id
        self.call_id = call_id
        self.turn_id = turn_id
        self.tool_name = tool_name


class SourceResultWithheld(AwError):
    """Raised when tool.after fails with on_error: withhold_result."""

    def __init__(
        self,
        event: str = "tool.after",
        *,
        session_id: str,
        call_id: str | None = None,
        tool_name: str | None = None,
    ) -> None:
        super().__init__("source_result_withheld", unavailable=False)
        self.event = event
        self.session_id = session_id
        self.call_id = call_id
        self.tool_name = tool_name
        self.tool_executed = True


def source_capabilities(
    events: Iterable[str] = DEFAULT_EVENTS,
    *,
    replace_input: bool = False,
    replace_result: bool = False,
    withhold_result: bool = False,
) -> dict[str, Any]:
    """Build canonical aw-hook-source/v1 capabilities for the selected events."""
    selected = list(dict.fromkeys(events))
    if not selected or any(
        not isinstance(event, str) or event not in SUPPORTED_EVENTS for event in selected
    ):
        raise AwError("semantic_event_unavailable")
    if replace_input and "tool.before" not in selected:
        raise AwError("semantic_event_unavailable")
    if (replace_result or withhold_result) and "tool.after" not in selected:
        raise AwError("semantic_event_unavailable")
    declared: dict[str, list[str]] = {event: list(SUPPORTED_EVENTS[event]) for event in selected}
    if replace_input:
        declared["tool.before"].append("replace_input")
    if replace_result:
        declared["tool.after"].append("replace_result")
    caps: dict[str, Any] = {
        "adapter": "sdk",
        "version": SOURCE_PROTOCOL,
        "entrypoint": "application",
        "events": declared,
    }
    if withhold_result:
        caps["failure_actions"] = {"tool.after": ["withhold_result"]}
    return caps


def confirm_source_routes(capabilities: Mapping[str, Any], routes: Mapping[str, Any]) -> bool:
    """Confirm that every route matches declared semantic events, effects and failure actions."""
    events = capabilities.get("events")
    failure_actions = capabilities.get("failure_actions") or {}
    hooks = routes.get("hooks") if isinstance(routes, Mapping) else None
    if (
        not isinstance(events, Mapping)
        or not isinstance(failure_actions, Mapping)
        or not isinstance(hooks, list)
    ):
        return False
    for hook in hooks:
        if not isinstance(hook, Mapping) or hook.get("skill_check"):
            return False
        event = hook.get("event")
        if event not in events:
            return False
        allowed_effects = events.get(event) or ()
        if hook.get("replace_input") and "replace_input" not in allowed_effects:
            return False
        if hook.get("replace_result") and "replace_result" not in allowed_effects:
            return False
        if hook.get("on_error") == "withhold_result" and "withhold_result" not in (
            failure_actions.get(event) or ()
        ):
            return False
    return True


def route_has_strict_after(routes: Mapping[str, Any] | None) -> bool:
    """Return whether admitted tool.after routes require replacement or withholding."""
    hooks = routes.get("hooks") if isinstance(routes, Mapping) else None
    if not isinstance(hooks, list):
        return False
    return any(
        isinstance(hook, Mapping)
        and hook.get("event") == "tool.after"
        and (bool(hook.get("replace_result")) or hook.get("on_error") == "withhold_result")
        for hook in hooks
    )
