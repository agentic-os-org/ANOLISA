"""Optional compatibility shim delegating to the anolisa-aw-sdk-qwenpaw package."""

from importlib import import_module
from types import ModuleType
from typing import Any

__all__ = ["AfterOnlyPlugin", "QwenPawPlugin", "prepare_runtime"]


def _load_optional() -> ModuleType:
    try:
        return import_module("aw_sdk_qwenpaw")
    except ModuleNotFoundError as error:
        if error.name in {"aw_sdk_qwenpaw", "aw_sdk_qwenpaw._native"}:
            raise ModuleNotFoundError(
                "Optional QwenPaw adapter package 'anolisa-aw-sdk-qwenpaw' is not installed. "
                "Install 'anolisa-aw-sdk-qwenpaw' alongside 'anolisa-aw-sdk' "
                "or import directly from 'aw_sdk_qwenpaw'.",
                name="aw_sdk_qwenpaw",
            ) from error
        raise


def prepare_runtime() -> bool:
    """Delegate optional native path acceleration to anolisa-aw-sdk-qwenpaw."""
    return _load_optional().prepare_runtime()


def __getattr__(name: str) -> Any:
    if name in ("AfterOnlyPlugin", "QwenPawPlugin"):
        return getattr(_load_optional(), name)
    raise AttributeError(name)
