"""Typer commands for observability ingestion."""

import json
import sys
from typing import Any

import typer
from agent_sec_cli.observability import record_observability
from agent_sec_cli.observability.schema import (
    ObservabilityRecord,
    observability_record_json_schema,
    validate_observability_record,
)
from pydantic import ValidationError

app = typer.Typer(help="Record observability metrics.")

_INPUT_FORMAT = "json"


class ObservabilityCliError(ValueError):
    """User-facing observability CLI validation error."""


def _validation_message(exc: ValidationError) -> str:
    errors = exc.errors()
    if not errors:
        return str(exc)
    message = str(errors[0].get("msg", exc))
    return message.removeprefix("Value error, ")


def _parse_record(value: Any) -> ObservabilityRecord:
    if not isinstance(value, dict):
        raise ObservabilityCliError("payload must be a JSON object")
    try:
        return validate_observability_record(value)
    except ValidationError as exc:
        raise ObservabilityCliError(_validation_message(exc)) from exc


def _parse_json(raw: str) -> ObservabilityRecord:
    if not raw.strip():
        raise ObservabilityCliError("stdin is empty")
    try:
        return _parse_record(json.loads(raw))
    except json.JSONDecodeError as exc:
        raise ObservabilityCliError(f"invalid JSON: {exc.msg}") from exc


@app.command()
def record(
    format_: str = typer.Option("json", "--format", help="Input format: json."),
    use_stdin: bool = typer.Option(False, "--stdin", help="Read payload from stdin."),
) -> None:
    """Record one observability JSON object from stdin.

    Required wire fields: hook, observedAt, metadata, metrics.
    Unknown top-level fields, metadata fields, and metric keys are ignored for
    forward compatibility. If no supported metrics remain, the record is rejected.
    """
    if format_ != _INPUT_FORMAT:
        typer.echo("Error: --format must be json.", err=True)
        raise typer.Exit(code=1)

    if not use_stdin:
        typer.echo("Error: --stdin is required.", err=True)
        raise typer.Exit(code=1)

    raw = sys.stdin.read()
    try:
        record_payload = _parse_json(raw)
    except ObservabilityCliError as exc:
        typer.echo(f"Error: {exc}", err=True)
        raise typer.Exit(code=1)

    try:
        record_observability(record_payload)
    except Exception as exc:  # noqa: BLE001
        typer.echo(f"Error: failed to write observability record: {exc}", err=True)
        raise typer.Exit(code=1) from exc
    raise typer.Exit(code=0)


@app.command(name="schema")
def schema_command() -> None:
    """Print the public observability record JSON Schema."""
    typer.echo(
        json.dumps(
            observability_record_json_schema(),
            indent=2,
            ensure_ascii=False,
        )
    )


@app.command()
def review() -> None:
    """Open an interactive drill-down TUI over recorded observability events."""
    if not sys.stdin.isatty() or not sys.stdout.isatty():
        typer.echo(
            "Error: `observability review` requires an interactive terminal. ",
            err=True,
        )
        raise typer.Exit(code=2)

    # Lazy-import Textual so the hot `record` / `schema` paths don't pay its
    # import cost.
    from agent_sec_cli.observability.correlation import (  # noqa: PLC0415
        SecurityCorrelationService,
    )
    from agent_sec_cli.observability.review import (  # noqa: PLC0415
        ObservabilityReviewApp,
    )
    from agent_sec_cli.observability.sqlite_reader import (  # noqa: PLC0415
        ObservabilityReader,
    )
    from agent_sec_cli.security_events.sqlite_reader import (  # noqa: PLC0415
        SqliteEventReader,
    )

    reader = ObservabilityReader()
    security_reader = None
    try:
        security_reader = SqliteEventReader()
        security_correlation = SecurityCorrelationService(security_reader)
        ObservabilityReviewApp(
            reader=reader,
            security_correlation=security_correlation,
        ).run()
    finally:
        if security_reader is not None:
            security_reader.close()
        reader.close()


@app.command()
def report(
    session_id: str = typer.Option(
        None, "--session-id", help="Session ID to report on."
    ),
    last: bool = typer.Option(
        False, "--last", help="Report on the most recent session."
    ),
    format_: str = typer.Option(
        "text", "--format", help="Output format: text or json."
    ),
) -> None:
    """Print a per-session debrief (LLM calls, tools, security)."""
    if format_ not in ("text", "json"):
        typer.echo(
            f"Error: --format must be 'text' or 'json', got '{format_}'.", err=True
        )
        raise typer.Exit(code=1)
    if not session_id and not last:
        typer.echo("Error: specify --session-id or --last.", err=True)
        raise typer.Exit(code=1)

    from agent_sec_cli.observability.session_report import (  # noqa: PLC0415
        build_session_report,
        format_text,
    )
    from agent_sec_cli.observability.sqlite_reader import (  # noqa: PLC0415
        ObservabilityReader,
    )
    from agent_sec_cli.security_events.sqlite_reader import (  # noqa: PLC0415
        SqliteEventReader,
    )

    reader = ObservabilityReader()
    security_reader = None
    try:
        if last:
            sessions = reader.list_sessions()
            if not sessions:
                typer.echo("No sessions recorded.", err=True)
                raise typer.Exit(code=1)
            session_id = sessions[0].session_id

        try:
            security_reader = SqliteEventReader()
        except Exception:
            pass

        rpt = build_session_report(session_id, reader, security_reader)

        if rpt is None:
            typer.echo(f"Error: session '{session_id}' not found.", err=True)
            raise typer.Exit(code=1)

        if format_ == "json":
            typer.echo(json.dumps(rpt.to_dict(), indent=2, ensure_ascii=False))
        else:
            typer.echo(format_text(rpt))
    finally:
        if security_reader is not None:
            security_reader.close()
        reader.close()


__all__ = ["app"]
