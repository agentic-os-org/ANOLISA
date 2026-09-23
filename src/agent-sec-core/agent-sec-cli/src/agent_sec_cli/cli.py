"""CLI entry point for agent-sec-cli package."""

import json
import sys
import time
from datetime import datetime, timezone
from typing import Any

import typer
from agent_sec_cli.capabilities.cli import app as capabilities_app
from agent_sec_cli.cli_logging import setup_cli_logging
from agent_sec_cli.correlation_context import (
    init_invocation_context,
    init_process_trace_context,
    parse_trace_context,
)
from agent_sec_cli.observability.cli import app as observability_app
from agent_sec_cli.pii_checker.cli import scanner_app as pii_scanner_app
from agent_sec_cli.prompt_scanner.cli import scanner_app
from agent_sec_cli.security_events import get_reader
from agent_sec_cli.security_events.summary_formatter import format_summary
from agent_sec_cli.security_middleware import invoke
from agent_sec_cli.security_middleware.backends.hardening import (
    DEFAULT_HARDEN_CONFIG,
)
from agent_sec_cli.security_middleware.lifecycle import _ACTION_CATEGORY
from agent_sec_cli.skill_ledger.cli import app as skill_ledger_app
from agent_sec_cli.utils.timestamp import (
    epoch_to_utc_iso,
    optional_iso_to_utc_iso,
)

# Get version from package metadata
try:
    from importlib.metadata import version as get_version

    __version__ = get_version("agent-sec-cli")
except Exception:
    __version__ = "0.13.1"  # pragma: no cover

app = typer.Typer(
    name="agent-sec-cli",
    help="AgentSecCore unified CLI entry point",
    add_completion=True,
    rich_markup_mode="rich",
)


def _extract_trace_context_arg(argv: list[str]) -> str | None:
    """Return hidden top-level trace context before Typer/logging setup.

    This bootstrap parser is the canonical trace-context parser. It only
    recognizes process-level options before the first command token so command
    arguments and downstream pass-through flags keep their own semantics.
    """
    trace_context: str | None = None
    index = 1 if argv else 0
    while index < len(argv):
        arg = argv[index]
        if arg == "--":
            return trace_context
        if arg == "--trace-context":
            if index + 1 < len(argv):
                value = argv[index + 1]
                if value.startswith("-"):
                    raise ValueError("missing trace context value")
                trace_context = value if value.strip() else None
                index += 2
                continue
            raise ValueError("missing trace context value")
        prefix = "--trace-context="
        if arg.startswith(prefix):
            value = arg[len(prefix) :]
            trace_context = value if value.strip() else None
            index += 1
            continue
        if not arg.startswith("-"):
            return trace_context
        index += 1
    return trace_context


def _init_trace_context(trace_context: str | None) -> None:
    """Initialize process trace context from raw CLI JSON."""
    init_process_trace_context(parse_trace_context(trace_context))


def _is_read_only_skill_analyze(argv: list[str]) -> bool:
    """Return whether argv selects the side-effect-free Skill analysis path."""
    command_tokens: list[str] = []
    index = 1 if argv else 0
    while index < len(argv) and len(command_tokens) < 2:
        arg = argv[index]
        if arg == "--trace-context":
            index += 2
            continue
        if arg.startswith("--trace-context="):
            index += 1
            continue
        if arg.startswith("-"):
            return False
        command_tokens.append(arg)
        index += 1
    return command_tokens == ["skill-ledger", "analyze"]


@app.callback(invoke_without_command=True)
def main_callback(
    ctx: typer.Context,
    version: bool = typer.Option(
        False,
        "--version",
        "-v",
        is_eager=True,
        help="Show version and exit.",
    ),
    trace_context: str | None = typer.Option(
        None,
        "--trace-context",
        help="JSON tracing context for plugin integrations.",
        hidden=True,
    ),
) -> None:
    """Main callback for version option."""
    # Declared but intentionally unused here so Typer recognizes the hidden
    # top-level option; process trace context is initialized once in main().

    if version:
        typer.echo(f"agent-sec-cli {__version__}")
        raise typer.Exit()


# Mount skill-ledger as a subcommand group: agent-sec-cli skill-ledger <cmd>
app.add_typer(skill_ledger_app, name="skill-ledger")
app.add_typer(observability_app, name="observability")
app.add_typer(capabilities_app, name="capabilities")

# ---------------------------------------------------------------------------
# Command: harden
# ---------------------------------------------------------------------------
_HARDEN_HELP_TEXT = f"""\
Usage: agent-sec-cli harden [SEHARDEN_ARGS]...

Defaults:
  If omitted, the wrapper adds `--scan --config {DEFAULT_HARDEN_CONFIG}`.

Examples:
  agent-sec-cli harden --scan --config {DEFAULT_HARDEN_CONFIG}
  agent-sec-cli harden --reinforce --config {DEFAULT_HARDEN_CONFIG}
  agent-sec-cli harden --reinforce --dry-run --config {DEFAULT_HARDEN_CONFIG}

Common SEHarden flags:
  --scan              Run compliance scan.
  --reinforce         Apply remediation actions.
  --dry-run           Preview reinforce actions without changing the system.
  --config <ruleset>  Select a profile name or YAML file.
  --level <level>     Limit execution to a profile level.
  --verbose           Show detailed rule-level evidence.
  --log-level <lv>    Set log level: trace|debug|info|warn|error.

Help:
  agent-sec-cli harden --help             Show this concise wrapper help.
  agent-sec-cli harden --downstream-help  Show full `loongshield seharden` help.
"""


def _with_default_harden_args(args: list[str]) -> list[str]:
    """Add wrapper defaults when the caller does not provide them explicitly."""
    normalized = list(args)
    if (
        "--scan" not in normalized
        and "--reinforce" not in normalized
        and "--dry-run" not in normalized
    ):
        normalized.insert(0, "--scan")
    if "--config" not in normalized and not any(
        arg.startswith("--config=") for arg in normalized
    ):
        normalized.extend(["--config", DEFAULT_HARDEN_CONFIG])
    return normalized


# Register prompt scanner sub-command
app.add_typer(scanner_app, name="scan-prompt")
# Register PII scanner sub-command
app.add_typer(pii_scanner_app, name="scan-pii")


# ---------------------------------------------------------------------------
# Command: log-sandbox (internal — called by sandbox-guard.py)
# ---------------------------------------------------------------------------
@app.command(name="log-sandbox", hidden=True)
def log_sandbox(
    decision: str = typer.Option(
        "",
        "--decision",
        help="Sandbox decision (allow/block/sandbox)",
    ),
    command: str = typer.Option(
        "",
        "--command",
        help="Command being evaluated",
    ),
    reasons: str = typer.Option(
        "",
        "--reasons",
        help="Reasons for the decision",
    ),
    network_policy: str = typer.Option(
        "",
        "--network-policy",
        help="Network policy applied",
    ),
    cwd: str = typer.Option(
        "",
        "--cwd",
        help="Current working directory",
    ),
) -> None:
    """Internal: Record sandbox prehook decision (called by sandbox-guard.py)."""
    result = invoke(
        "sandbox_prehook",
        decision=decision,
        command=command,
        reasons=reasons,
        network_policy=network_policy,
        cwd=cwd,
    )
    # Silent exit - async call doesn't need output
    raise typer.Exit(code=result.exit_code)


@app.command(
    short_help="Scan or reinforce the system against a security baseline.",
    context_settings={
        "allow_extra_args": True,
        "ignore_unknown_options": True,
        "help_option_names": [],
    },
)
def harden(
    ctx: typer.Context,
    help_flag: bool = typer.Option(
        False,
        "--help",
        "-h",
        is_eager=True,
        help="Show concise harden help and examples.",
    ),
    downstream_help: bool = typer.Option(
        False,
        "--downstream-help",
        help="Show full `loongshield seharden` help and exit.",
    ),
) -> None:
    """Scan or reinforce the system against a security baseline."""
    if help_flag:
        typer.echo(_HARDEN_HELP_TEXT.rstrip())
        raise typer.Exit(code=0)

    if downstream_help:
        result = invoke("harden", args=["--help"])
    else:
        result = invoke("harden", args=_with_default_harden_args(list(ctx.args)))

    if result.stdout:
        typer.echo(result.stdout, nl=False)
    if result.error:
        typer.echo(result.error, err=True)
    raise typer.Exit(code=result.exit_code)


# ---------------------------------------------------------------------------
# Command: verify
# ---------------------------------------------------------------------------
@app.command()
def verify(
    skill: str = typer.Option(
        None,
        "--skill",
        help="Path to specific skill for verification",
    ),
) -> None:
    """Skill integrity verification."""
    result = invoke("verify", skill=skill)
    if result.stdout:
        typer.echo(result.stdout, nl=False)
    if result.error:
        typer.echo(result.error, err=True)
    raise typer.Exit(code=result.exit_code)


# ---------------------------------------------------------------------------
# Command: scan-code
# ---------------------------------------------------------------------------
@app.command(name="scan-code")
def code_scan(
    code: str = typer.Option("", "--code", help="Source code to scan"),
    language: str = typer.Option("bash", "--language", help="Language: bash or python"),
    mode: str = typer.Option("regex", "--mode", help="Engine mode: regex or llm"),
) -> None:
    """Scan code for security issues."""
    if not code.strip():
        typer.echo("Error: --code is required (use --code '<source>')", err=True)
        raise typer.Exit(code=1)
    result = invoke("code_scan", code=code, language=language, mode=mode)
    if result.stdout:
        typer.echo(result.stdout)
    if result.error:
        typer.echo(result.error, err=True)
    raise typer.Exit(code=result.exit_code)


# ---------------------------------------------------------------------------
# Command: events
#
# Examples:
#   # List recent events (default: table format, last 100)
#   agent-sec-cli events --last-hours 24
#
#   # Filter by type and show as JSON
#   agent-sec-cli events --event-type harden --output json
#
#   # Count hardening events in the last 8 hours
#   agent-sec-cli events --count --category hardening --last-hours 8
#
#   # Breakdown by category
#   agent-sec-cli events --count-by category --last-hours 24
#
#   # Paginate: skip first 50, show next 20
#   agent-sec-cli events --offset 50 --limit 20
#
#   # Stream events for scripting (one JSON object per line)
#   agent-sec-cli events --last-hours 1 --output jsonl | jq '.result'
# ---------------------------------------------------------------------------

_COUNT_BY_ALLOWED = {"category", "event_type", "trace_id"}
_OUTPUT_FORMATS = {"table", "json", "jsonl"}

# Canonical event types and categories — dynamically derived from lifecycle.py
# _ACTION_CATEGORY mapping to ensure automatic synchronization.
# Keys → event types, Values → categories
# Note: Success/failure is tracked via the `result` field (succeeded/failed),
# NOT via _error suffix in event_type. This keeps the event model clean and
# makes it easy to query all events of a type regardless of outcome.
_VALID_EVENT_TYPES = set(_ACTION_CATEGORY.keys())

_VALID_CATEGORIES = set(_ACTION_CATEGORY.values())


def _resolve_time_range(
    last_hours: float | None,
    since: str | None,
    until: str | None,
) -> tuple[str | None, str | None]:
    """Resolve since/until from either explicit values or last_hours.

    Ensures consistent time range handling across all query modes.

    Raises
    ------
    ValueError
        If since or until parameters are not valid ISO-8601 format.
    """
    if last_hours is not None:
        now = time.time()
        since_epoch = now - last_hours * 3600
        # Relative ranges are epoch-based already; emit UTC before querying SQLite.
        since = epoch_to_utc_iso(since_epoch)
        until = epoch_to_utc_iso(now)
    else:
        # Explicit CLI timestamps may be naive local time; normalize at the CLI boundary.
        since = optional_iso_to_utc_iso(since, field_name="--since")
        until = optional_iso_to_utc_iso(until, field_name="--until")
    return since, until


def _format_timestamp(ts: str) -> str:
    """Render an ISO-8601 timestamp in local time for table display.

    Reduces column width from ~32 to 19 characters while preserving
    all meaningful precision for security event browsing.
    """
    try:
        dt = datetime.fromisoformat(ts)
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        return dt.astimezone().strftime("%Y-%m-%d %H:%M:%S")
    except (ValueError, TypeError):
        return ts  # Fallback to original if parsing fails


def _format_table(events_list: list[Any]) -> str:
    """Format events as a kubectl-style columnar table.

    Column widths are computed dynamically from the data (like kubectl)
    so that long values never bleed into adjacent columns.
    """
    if not events_list:
        return "No events found."

    headers = ("EVENT_TYPE", "CATEGORY", "RESULT", "TIMESTAMP")
    rows = [
        (
            e.to_dict().get("event_type", ""),
            e.to_dict().get("category", ""),
            e.to_dict().get("result", "succeeded"),
            _format_timestamp(e.to_dict().get("timestamp", "")),
        )
        for e in events_list
    ]

    # Compute column widths: max(header, all values) + 2 padding
    col_widths = [
        max(len(h), *(len(r[i]) for r in rows)) + 2 for i, h in enumerate(headers)
    ]

    lines: list[str] = []
    lines.append("".join(h.ljust(w) for h, w in zip(headers, col_widths)).rstrip())
    for row in rows:
        lines.append("".join(v.ljust(w) for v, w in zip(row, col_widths)).rstrip())

    count = len(events_list)
    lines.append("")
    lines.append(f"{count} event{'s' if count != 1 else ''}")

    return "\n".join(lines)


@app.command()
def events(
    event_type: str | None = typer.Option(
        None,
        "--event-type",
        help=(
            "Filter by event type. "
            f"Known types: {', '.join(sorted(_VALID_EVENT_TYPES))}. "
            "(Success/failure is tracked in the RESULT column, not in event_type)"
        ),
    ),
    category: str | None = typer.Option(
        None,
        "--category",
        help=(
            "Filter by category. "
            f"Known categories: {', '.join(sorted(_VALID_CATEGORIES))}."
        ),
    ),
    trace_id: str | None = typer.Option(None, "--trace-id", help="Filter by trace ID."),
    session_id: str | None = typer.Option(
        None, "--session-id", help="Filter by session ID."
    ),
    run_id: str | None = typer.Option(None, "--run-id", help="Filter by run ID."),
    since: str | None = typer.Option(
        None, "--since", help="Inclusive lower bound (ISO-8601 timestamp)."
    ),
    until: str | None = typer.Option(
        None, "--until", help="Exclusive upper bound (ISO-8601 timestamp)."
    ),
    last_hours: float | None = typer.Option(
        None,
        "--last-hours",
        help="Query events from the last N hours (mutually exclusive with --since/--until).",
    ),
    limit: int = typer.Option(100, "--limit", help="Max results (default 100)."),
    offset: int = typer.Option(0, "--offset", help="Skip N results (default 0)."),
    count: bool = typer.Option(
        False, "--count", help="Output only the count of matching events."
    ),
    count_by: str | None = typer.Option(
        None,
        "--count-by",
        help="Output grouped counts as JSON object. Allowed: category, event_type, trace_id.",
    ),
    output: str | None = typer.Option(
        None,
        "--output",
        "-o",
        help="Output format: table (default, human-readable), json, jsonl.",
    ),
    summary: bool = typer.Option(
        False,
        "--summary",
        help=(
            "Output a human-readable security posture summary. "
            "Incompatible with --count, --count-by, --output."
        ),
    ),
) -> None:
    """Query security events from the local SQLite store."""
    # TODO: Support paging with limit and continue

    # --- validation ---
    if summary and (count or count_by is not None):
        typer.echo(
            "Error: --summary is incompatible with --count and --count-by.",
            err=True,
        )
        raise typer.Exit(code=1)

    # --summary is incompatible with ANY explicit --output format
    if summary and output is not None:
        typer.echo(
            "Error: --summary is incompatible with --output (summary has its own format).",
            err=True,
        )
        raise typer.Exit(code=1)

    # Apply default output format if not specified
    if output is None:
        output = "table"

    if output not in _OUTPUT_FORMATS:
        typer.echo(
            f"Error: --output must be one of: {', '.join(sorted(_OUTPUT_FORMATS))}.",
            err=True,
        )
        raise typer.Exit(code=1)

    if event_type is not None and event_type not in _VALID_EVENT_TYPES:
        typer.echo(
            f"Warning: Unknown event_type '{event_type}'. "
            f"Known types: {', '.join(sorted(_VALID_EVENT_TYPES))}",
            err=True,
        )
        # Don't reject — allow future event types, just warn

    if category is not None and category not in _VALID_CATEGORIES:
        typer.echo(
            f"Warning: Unknown category '{category}'. "
            f"Known categories: {', '.join(sorted(_VALID_CATEGORIES))}",
            err=True,
        )
        # Don't reject — allow future categories, just warn

    if last_hours is not None and last_hours < 0:
        typer.echo("Error: --last-hours must be non-negative.", err=True)
        raise typer.Exit(code=1)

    if limit <= 0:
        typer.echo("Error: --limit must be positive.", err=True)
        raise typer.Exit(code=1)

    if offset < 0:
        typer.echo("Error: --offset must be non-negative.", err=True)
        raise typer.Exit(code=1)

    if last_hours is not None and (since is not None or until is not None):
        typer.echo(
            "Error: --last-hours is mutually exclusive with --since/--until.",
            err=True,
        )
        raise typer.Exit(code=1)

    if count and count_by is not None:
        typer.echo("Error: --count and --count-by are mutually exclusive.", err=True)
        raise typer.Exit(code=1)

    if count_by is not None and count_by not in _COUNT_BY_ALLOWED:
        typer.echo(
            f"Error: --count-by must be one of: {', '.join(sorted(_COUNT_BY_ALLOWED))}.",
            err=True,
        )
        raise typer.Exit(code=1)

    reader = get_reader()

    # --- summary mode ---
    if summary:
        # Default to last 24 hours if no time range specified
        summary_hours = last_hours if last_hours is not None else None
        if summary_hours is None and since is None and until is None:
            summary_hours = 24.0

        try:
            resolved_since, resolved_until = _resolve_time_range(
                summary_hours, since, until
            )
        except ValueError as exc:
            typer.echo(f"Error: {exc}", err=True)
            raise typer.Exit(code=1)

        events_list = reader.query(
            event_type=event_type,
            category=category,
            trace_id=trace_id,
            session_id=session_id,
            run_id=run_id,
            since=resolved_since,
            until=resolved_until,
            limit=10000,
            offset=0,
        )
        time_label = (
            f"last {summary_hours:.0f} hours"
            if summary_hours is not None
            else f"{since or '...'} to {until or 'now'}"
        )

        typer.echo(format_summary(events_list, time_label))
        raise typer.Exit(code=0)

    # --- count mode ---
    if count:
        try:
            resolved_since, resolved_until = _resolve_time_range(
                last_hours, since, until
            )
        except ValueError as exc:
            typer.echo(f"Error: {exc}", err=True)
            raise typer.Exit(code=1)

        result = reader.count(
            event_type=event_type,
            category=category,
            trace_id=trace_id,
            session_id=session_id,
            run_id=run_id,
            since=resolved_since,
            until=resolved_until,
            offset=offset,
        )
        typer.echo(json.dumps(result, ensure_ascii=False, indent=2))
        raise typer.Exit(code=0)

    # --- count-by mode ---
    if count_by is not None:
        try:
            resolved_since, resolved_until = _resolve_time_range(
                last_hours, since, until
            )
        except ValueError as exc:
            typer.echo(f"Error: {exc}", err=True)
            raise typer.Exit(code=1)

        result = reader.count_by(
            count_by,
            event_type=event_type,
            category=category,
            trace_id=trace_id,
            session_id=session_id,
            run_id=run_id,
            since=resolved_since,
            until=resolved_until,
            offset=offset,
        )
        typer.echo(json.dumps(result, ensure_ascii=False, indent=2))
        raise typer.Exit(code=0)

    # --- list mode ---
    try:
        resolved_since, resolved_until = _resolve_time_range(last_hours, since, until)
    except ValueError as exc:
        typer.echo(f"Error: {exc}", err=True)
        raise typer.Exit(code=1)

    events_list = reader.query(
        event_type=event_type,
        category=category,
        trace_id=trace_id,
        session_id=session_id,
        run_id=run_id,
        since=resolved_since,
        until=resolved_until,
        limit=limit,
        offset=offset,
    )

    if output == "table":
        typer.echo(_format_table(events_list))
    elif output == "json":
        output_data = [e.to_dict() for e in events_list]
        typer.echo(json.dumps(output_data, ensure_ascii=False, indent=2))
    elif output == "jsonl":
        for e in events_list:
            typer.echo(json.dumps(e.to_dict(), ensure_ascii=False))
    raise typer.Exit(code=0)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main() -> None:
    """Main entry point."""
    try:
        # Preload tracing before Typer executes callbacks so future CLI logging
        # setup can correlate startup records. This is the single process-level
        # trace-context initialization path.
        _init_trace_context(_extract_trace_context_arg(sys.argv))
        init_invocation_context()
        if not _is_read_only_skill_analyze(sys.argv):
            setup_cli_logging()
    except ValueError as exc:
        typer.echo(f"Error: {exc}", err=True)
        raise SystemExit(1) from exc
    app()


if __name__ == "__main__":
    main()
