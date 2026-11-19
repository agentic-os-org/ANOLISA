"""Optional compatibility shim delegating to the anolisa-aw-sdk-claude package."""

from importlib import import_module
from types import ModuleType
from typing import Any

__all__ = ["ResidentClaude", "SubmissionDenied", "create_sdk_mcp_server"]


def _load_optional() -> ModuleType:
    try:
        return import_module("aw_sdk_claude")
    except ModuleNotFoundError as error:
        if error.name in {"aw_sdk_claude", "aw_sdk_claude._native"}:
            raise ModuleNotFoundError(
                "Optional Claude adapter package 'anolisa-aw-sdk-claude' is not installed. "
                "Install 'anolisa-aw-sdk-claude' alongside 'anolisa-aw-sdk' "
                "or import directly from 'aw_sdk_claude'.",
                name="aw_sdk_claude",
            ) from error
        raise


def create_sdk_mcp_server(name: str, version: str = "1.0.0", tools: Any = None) -> Any:
    """Delegate to anolisa-aw-sdk-claude's managed in-process MCP server helper."""
    return _load_optional().create_sdk_mcp_server(name, version, tools)


def __getattr__(name: str) -> Any:
    if name in ("ResidentClaude", "SubmissionDenied"):
        return getattr(_load_optional(), name)
    raise AttributeError(name)
