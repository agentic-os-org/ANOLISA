"""CLI entry point for the prompt scanner (scan-prompt command).

The implementation delegates to ``agent_sec_cli.security_middleware.invoke``
so that every scan is recorded as a ``prompt_scan`` security event, consistent
with ``scan-pii`` and the other security commands.
"""

import json
import os
import sys
from pathlib import Path
from typing import Any

import typer
from agent_sec_cli.security_middleware import invoke
from agent_sec_cli.security_middleware.backends.prompt_scan import error_payload
from agent_sec_cli.security_middleware.result import ActionResult

_SUPPORTED_MODES = frozenset({"fast", "standard", "strict", "multi_turn"})
_MULTITURN_MODE = "multi_turn"
_L2_MODEL_ENV = "PROMPT_SCANNER_L2_MODEL"
# Modes whose pipeline includes the L2 ml_classifier layer; an L2 model
# override is inert in every other mode.
_L2_MODES = frozenset({"standard", "strict"})
# Modes whose warmup actually reaches Ollama: standard/strict probe the L2
# model, multi_turn the fixed L4 one.  fast is L1-only, so its warmup builds
# the rule engine and nothing else.
_MODEL_BACKED_MODES = _L2_MODES | {_MULTITURN_MODE}

# Selectable L2 backends, listed in the help epilog rather than in the
# ``--model`` help text: the option column is ~45 chars wide, so these 46-50
# char names get truncated with an ellipsis there and stop being copyable.
# The native layer owns the authoritative list (it rejects anything else at
# construction) and reports the same set through ``scanner_engine_info``.
_L2_BACKENDS_EPILOG = (
    f"L2 backends for --model / {_L2_MODEL_ENV}:\n\n"
    "modelscope.cn/ANOLISA/Qwen3Guard-Gen-0.6B-GGUF  (default)\n\n"
    "modelscope.cn/ANOLISA/Warden-Gen-0.6B-GGUF"
)

scanner_app = typer.Typer(
    name="scan-prompt",
    help="Prompt injection / jailbreak scanner",
    epilog=_L2_BACKENDS_EPILOG,
)


def _resolve_l2_model(cli_model: str | None = None) -> str | None:
    """Resolve the L2 backend override.

    Priority: ``--model`` > ``PROMPT_SCANNER_L2_MODEL`` > ``None`` (the
    scanner's built-in default). A blank or whitespace-only value at either
    layer means "not set" and falls through to the next one.

    Every plugin hook shells out to this command, so the environment variable
    lets all of them switch backends without a change of their own, while
    ``--model`` covers ad-hoc terminal use and per-invocation overrides.
    """
    if cli_model and cli_model.strip():
        return cli_model.strip()
    return os.environ.get(_L2_MODEL_ENV, "").strip() or None


def _warn_inert_l2_model(
    mode: str, model: str | None, resolved_model: str | None
) -> None:
    """Warn when an L2 backend override has no effect in ``mode``.

    ``--model`` / ``PROMPT_SCANNER_L2_MODEL`` only reconfigures the L2 layer,
    which runs in standard/strict.  fast (L1 only) and multi_turn (fixed L4
    model) ignore it entirely, so warn rather than let an operator mistake an
    inert override for a real backend switch when troubleshooting.
    """
    if resolved_model and mode not in _L2_MODES:
        origin = "--model" if (model and model.strip()) else _L2_MODEL_ENV
        typer.echo(
            f"Warning: {origin} '{resolved_model}' is ignored in {mode} mode; "
            "it only applies to standard/strict (L2).",
            err=True,
        )


def _print_error_json(message: str) -> None:
    """Print a scanner-compatible ERROR verdict payload."""
    typer.echo(json.dumps(error_payload(message), indent=2, ensure_ascii=False))


def _invoke_prompt_scan(**kwargs: Any) -> ActionResult:
    """Call the middleware, containing unexpected exceptions.

    The backend already converts scanner failures into ERROR verdicts; this
    guard covers anything escaping ``invoke`` itself (e.g. routing bugs) so
    automated consumers always receive the spec error JSON instead of a
    traceback.  Exits 1, matching the backend's ERROR exit code, so a caller
    cannot tell a contained failure from an escaped one by exit status.
    """
    try:
        return invoke("prompt_scan", **kwargs)
    except Exception as exc:  # noqa: BLE001 - CLI error surface
        _print_error_json(f"Scanner error: {exc}")
        raise typer.Exit(code=1)


def _print_result(result: ActionResult, output_format: str) -> None:
    """Print a middleware scan result in the requested format."""
    if output_format == "text":
        # ``data`` is a dict by contract, but guard against a malformed
        # result so display never crashes on ``None``.
        _print_text(result.data or {})
    else:
        typer.echo(result.stdout)


def _format_text(d: dict[str, Any]) -> str:
    """Format a scan result dict as human-readable text."""
    verdict = d.get("verdict", "unknown").upper()
    icon = {"PASS": "✅", "WARN": "⚠️", "DENY": "❌", "ERROR": "💥"}.get(verdict, "?")
    lines = [
        f"{icon}  Verdict : {verdict}",
        f"    Risk    : {d.get('risk_level', 'unknown')} "
        f"(score: {d.get('confidence', 0):.3f})",
        f"    Threat  : {d.get('threat_type', 'unknown')}",
        f"    Summary : {d.get('summary', '')}",
    ]
    findings = d.get("findings") or []
    if findings:
        lines.append("    Findings:")
        for f in findings:
            lines.append(f"      {f.get('rule_id', '?')} — {f.get('title', '')}")
            evidence = f.get("evidence")
            if evidence:
                lines.append(f"        evidence: {evidence[:80]!r}")
    # `elapsed_ms` is the total; break it out when engine construction paid a
    # cold-start cost, so a slow invocation points at the rule-set compile
    # rather than looking like a slow scan.
    elapsed = d.get("elapsed_ms", 0)
    engine_init = d.get("engine_init_ms") or 0
    if engine_init:
        lines.append(
            f"    Elapsed : {elapsed} ms "
            f"(engine init {engine_init}, scan {d.get('scan_ms', 0)})"
        )
    else:
        lines.append(f"    Elapsed : {elapsed} ms")
    return "\n".join(lines)


def _print_text(d: dict[str, Any]) -> None:
    """Print a scan result in human-readable text format."""
    typer.echo(_format_text(d))


def _load_native() -> Any:
    """Import the Rust native scanner module lazily.

    The extension is built by ``maturin develop/build``.  Importing lazily
    lets ``--help`` and tab completion work even before the extension is
    compiled.
    """
    from agent_sec_cli import _native  # noqa: PLC0415 - lazy import by design

    return _native


@scanner_app.command("warmup", epilog=_L2_BACKENDS_EPILOG)
def warmup_model(
    mode: str = typer.Option(
        "standard",
        "--mode",
        help="Detection mode to check: fast, standard, strict, multi_turn",
        case_sensitive=False,
    ),
    model: str | None = typer.Option(
        None,
        "--model",
        help="L2 backend model to check; see the backend list below. "
        f"Overrides {_L2_MODEL_ENV}.",
    ),
) -> None:
    """Check that Ollama can serve the models the selected mode requires.

    fast requires none, so there the check only covers the rule engine.

    Availability only: a model Ollama can serve is reported ready without
    being loaded into memory, so the first scan still pays the model's
    cold-start cost.

    Models are never downloaded automatically; pull them into Ollama first
    (e.g. ``ollama pull <model>``).
    """
    mode = mode.lower()
    if mode not in _SUPPORTED_MODES:
        typer.echo(
            f"Error: Invalid mode '{mode}'. "
            "Choose from: fast, standard, strict, multi_turn",
            err=True,
        )
        raise typer.Exit(code=1)

    resolved_model = _resolve_l2_model(model)
    _warn_inert_l2_model(mode, model, resolved_model)

    try:
        native = _load_native()
        native.warmup_scanner(mode=mode, model=resolved_model)
    except Exception as exc:  # noqa: BLE001 - CLI error surface
        typer.echo(f"Model check failed: {exc}", err=True)
        raise typer.Exit(code=1)

    if mode in _MODEL_BACKED_MODES:
        typer.echo("Check complete. Ollama can serve the model.")
    else:
        typer.echo(
            f"Check complete. {mode} mode runs rules only; no model was checked."
        )


@scanner_app.callback(invoke_without_command=True)
def scan_prompt(
    ctx: typer.Context,
    mode: str = typer.Option(
        "standard",
        "--mode",
        help="Detection mode: fast (L1), standard (L1+L2), strict (L1+L2+L3 reserved), multi_turn (L4, reads JSON from stdin)",
        case_sensitive=False,
    ),
    output_format: str = typer.Option(
        "json",
        "--format",
        help="Output format: 'json' (default) or 'text' (human-readable)",
    ),
    source: str = typer.Option(
        "",
        "--source",
        help="Label for the input origin (e.g. user_input, rag, tool_output)",
    ),
    text: str | None = typer.Option(
        None,
        "--text",
        help="Prompt text to scan directly. Takes precedence over --input and stdin.",
    ),
    input_file: str | None = typer.Option(
        None,
        "--input",
        help="Path to a file containing prompts (one per line). "
        "If omitted, reads from stdin.",
    ),
    model: str | None = typer.Option(
        None,
        "--model",
        help="L2 backend model; see the backend list below. "
        f"Overrides {_L2_MODEL_ENV}.",
    ),
) -> None:
    """Scan prompt text for injection / jailbreak attempts.

    Input priority: --text > --input <file> > stdin

    For multi_turn (L4) mode, pipe a JSON payload via stdin with
    {history, current_query, assistant_response}:

        echo '{"history":[...],"current_query":"...","assistant_response":"..."}' | \\
            agent-sec-cli scan-prompt --mode multi_turn

    Examples::

        # Direct text
        agent-sec-cli scan-prompt --text "ignore previous instructions"

        # Stdin (pipe)
        echo "ignore previous instructions" | agent-sec-cli scan-prompt

        # File
        agent-sec-cli scan-prompt --input prompts.txt --format json

        # Human-readable output
        agent-sec-cli scan-prompt --text "hello" --format text
    """
    # If a sub-command (e.g. warmup) was invoked, skip scan logic entirely.
    if ctx.invoked_subcommand is not None:
        return

    mode = mode.lower()
    if mode not in _SUPPORTED_MODES:
        typer.echo(
            f"Error: Invalid mode '{mode}'. "
            "Choose from: fast, standard, strict, multi_turn",
            err=True,
        )
        raise typer.Exit(code=1)

    if output_format not in ("json", "text"):
        typer.echo(
            f"Error: Invalid format '{output_format}'. Choose from: json, text",
            err=True,
        )
        raise typer.Exit(code=1)

    # Resolve the L2 backend once so every path (multi_turn and the per-line
    # batch below) uses the same model.
    resolved_model = _resolve_l2_model(model)
    _warn_inert_l2_model(mode, model, resolved_model)

    # --- MULTI_TURN mode: read JSON payload from stdin ---
    if mode == _MULTITURN_MODE:
        if text is not None or input_file:
            typer.echo(
                "Error: --text and --input are not supported with multi_turn mode. "
                "Pipe a JSON payload via stdin:\n"
                '  echo \'{"history":[...],"current_query":"...","assistant_response":"..."}\' | '
                "agent-sec-cli scan-prompt --mode multi_turn",
                err=True,
            )
            raise typer.Exit(code=1)

        raw = sys.stdin.read().strip()
        if not raw:
            typer.echo("Error: No input received from stdin.", err=True)
            raise typer.Exit(code=1)
        try:
            payload = json.loads(raw)
        except (json.JSONDecodeError, ValueError) as exc:
            typer.echo(f"Error: Invalid JSON: {exc}", err=True)
            raise typer.Exit(code=1)

        history = payload.get("history") or []
        current_query = payload.get("current_query") or ""
        assistant_response = payload.get("assistant_response") or ""
        if (
            not isinstance(history, list)
            or not isinstance(current_query, str)
            or not isinstance(assistant_response, str)
        ):
            typer.echo(
                "Error: payload must include a 'history' list, a 'current_query' "
                "string, and an 'assistant_response' string.",
                err=True,
            )
            raise typer.Exit(code=1)
        if not current_query.strip():
            typer.echo("Error: current_query is empty.", err=True)
            raise typer.Exit(code=1)

        result = _invoke_prompt_scan(
            text=current_query,
            assistant_response=assistant_response,
            history=history,
            mode=mode,
            source=source or None,
            model=resolved_model,
        )

        # L4 is mandatory in multi_turn mode, so an empty ``layer_results``
        # can only mean the scan itself failed (ERROR verdict) rather than a
        # pass-through.  ``data`` is a dict by contract; ``or {}`` guards a
        # malformed result, which means no detectors ran either.
        if not (result.data or {}).get("layer_results"):
            typer.echo(
                "Warning: no detection layer ran — the multi-turn scan did not "
                "complete (check that Ollama is reachable). Treat the verdict "
                "as unknown.",
                err=True,
            )

        _print_result(result, output_format)
        raise typer.Exit(code=result.exit_code)

    # --- Read input texts ---
    texts: list[str]
    if text is not None:
        # --text flag takes precedence; an empty string means "nothing to scan".
        if not text.strip():
            raise typer.Exit(code=0)
        texts = [text]
    elif input_file:
        try:
            with Path(input_file).open(encoding="utf-8") as fh:
                texts = [line.strip() for line in fh if line.strip()]
            if not texts:
                typer.echo(f"Error: File is empty: {input_file}", err=True)
                raise typer.Exit(code=1)
        except FileNotFoundError:
            typer.echo(f"Error: File not found: {input_file}", err=True)
            raise typer.Exit(code=1)
    else:
        raw = sys.stdin.read().strip()
        if not raw:
            typer.echo("Error: No input received from stdin.", err=True)
            raise typer.Exit(code=1)
        texts = [raw]

    # --- Scan each text through the middleware ---
    exit_code = 0
    for t in texts:
        result = _invoke_prompt_scan(
            text=t,
            mode=mode,
            source=source or None,
            model=resolved_model,
        )
        _print_result(result, output_format)
        if result.exit_code != 0:
            exit_code = result.exit_code

    raise typer.Exit(code=exit_code)
