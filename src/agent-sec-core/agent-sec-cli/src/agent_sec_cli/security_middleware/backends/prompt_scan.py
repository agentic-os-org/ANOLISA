"""prompt_scan backend — delegates to the native (Rust) prompt scanner."""

import json
import logging
from typing import Any

from agent_sec_cli.security_middleware.backends.base import BaseBackend
from agent_sec_cli.security_middleware.context import RequestContext
from agent_sec_cli.security_middleware.result import ActionResult

# Modes served by the native scanner: fast (L1), standard/strict (L1+L2),
# multi_turn (L4).  The legacy Python engine is fully removed; see git
# history for the retired implementation.
_SUPPORTED_MODES = frozenset({"fast", "standard", "strict", "multi_turn"})
_MULTI_TURN_MODE = "multi_turn"

_NATIVE_UNAVAILABLE_MSG = (
    "native prompt scanner is not available; "
    "rebuild the package with `maturin develop --release`"
)


class PromptScanBackend(BaseBackend):
    """Scan prompt text for injection / jailbreak attempts using the native scanner."""

    def execute(self, ctx: RequestContext, **kwargs: Any) -> ActionResult:
        text: str = kwargs.get("text", "")
        mode_str: str = kwargs.get("mode", "standard")
        source: str = kwargs.get("source", "")
        model: str = kwargs.get("model", "")

        if not text or not text.strip():
            return ActionResult(
                success=False,
                error="prompt_scan error: no input text provided",
                exit_code=1,
                error_type="ValueError",
            )

        mode = mode_str.lower()
        if mode not in _SUPPORTED_MODES:
            return ActionResult(
                success=False,
                error=f"prompt_scan error: invalid mode '{mode_str}'. "
                "Choose from: fast, standard, strict, multi_turn",
                exit_code=1,
                error_type="ValueError",
            )

        try:
            native = _load_native()
        except (ImportError, AttributeError):
            return _scanner_error_result(
                _NATIVE_UNAVAILABLE_MSG, error_type="NativeScannerUnavailable"
            )

        try:
            if mode == _MULTI_TURN_MODE:
                # L4 consumes a conversation triple; history is forwarded
                # as JSON so the native layer owns the parsing rules.
                history = kwargs.get("history") or []
                raw = native.scan_multi_turn_json(
                    text,
                    kwargs.get("assistant_response") or "",
                    history_json=json.dumps(history),
                    mode=mode,
                    source=source or None,
                    model=model or None,
                )
            else:
                raw = native.scan_prompt_json(
                    text,
                    mode=mode,
                    source=source or None,
                    model=model or None,
                )
            d = json.loads(raw)
        except Exception as exc:
            return _scanner_error_result(
                f"Scanner error: {exc}", error_type=type(exc).__name__
            )

        has_error = d.get("verdict") == "error"

        return ActionResult(
            success=not has_error,
            data=d,
            stdout=json.dumps(d, indent=2, ensure_ascii=False),
            exit_code=1 if has_error else 0,
            error_type="PromptScanError" if has_error else "",
        )


def _load_native() -> Any:
    """Return the native scanner module.

    Imported lazily so backend registration works even when the native
    extension has not been built; the failure is surfaced per-request as
    an ERROR verdict instead of an import-time crash.

    Raises:
        ImportError: the extension module is missing.
        AttributeError: the extension predates the scanner functions.
    """
    from agent_sec_cli import _native  # noqa: PLC0415 - lazy import by design

    # Touch both entry points so a stale build fails here rather than
    # halfway through a scan.
    _native.scan_prompt_json
    _native.scan_multi_turn_json
    return _native


def _engine_version() -> str:
    """Return the native engine version, or ``"unknown"`` when unavailable.

    Read from the native module so the version keeps a single source of truth
    (the ``prompt-scanner`` crate).  Error payloads are also built when the
    extension cannot be imported at all, hence the fallback.
    """
    try:
        info = json.loads(_load_native().scanner_engine_info())
        return str(info["engine_version"])
    except (ImportError, AttributeError):
        # Extension not built / stale build — expected during development.
        return "unknown"
    except Exception as exc:  # noqa: BLE001 - version probing must never raise
        # json.loads failure or missing key means the extension is broken or
        # mismatched; surface it as debug rather than swallowing silently.
        logging.getLogger(__name__).debug(
            "_engine_version: unexpected failure probing native extension: %s", exc
        )
        return "unknown"


def error_payload(message: str) -> dict[str, Any]:
    """Build the spec ERROR verdict payload (``schema_version: "1.0"``).

    Shared with the scan-prompt CLI so error output keeps a single shape
    regardless of where the failure is caught.  Every always-present
    field of the Rust ``to_json_value`` contract is emitted here: the
    timing trio (``elapsed_ms`` / ``engine_init_ms`` / ``scan_ms``, all
    zero, preserving the documented ``elapsed == init + scan`` identity)
    and the scan-completeness group (``input_truncated`` /
    ``input_bytes_scanned`` / ``degraded`` / ``layers_failed``), so
    consumers can gate on ``degraded`` without probing for keys.  Nothing
    was scanned on this path, so ``degraded`` is ``True`` (fail-safe: a
    hook gating on it applies its stricter policy instead of trusting an
    unscanned input), and ``layers_failed`` is empty because the failure
    is top-level, not per-layer -- the human-readable cause stays in
    ``summary``.
    """
    return {
        "schema_version": "1.0",
        "ok": False,
        "verdict": "error",
        "risk_level": "unknown",
        "threat_type": "unknown",
        "confidence": 0.0,
        "summary": message,
        "findings": [],
        "layer_results": [],
        "engine_version": _engine_version(),
        "elapsed_ms": 0,
        "engine_init_ms": 0,
        "scan_ms": 0,
        "input_truncated": False,
        "input_bytes_scanned": 0,
        "degraded": True,
        "layers_failed": [],
    }


def _scanner_error_result(
    message: str,
    *,
    error_type: str = "PromptScanError",
) -> ActionResult:
    data = error_payload(message)
    return ActionResult(
        success=False,
        data=data,
        stdout=json.dumps(data, indent=2, ensure_ascii=False),
        error=message,
        exit_code=1,
        error_type=error_type,
    )
