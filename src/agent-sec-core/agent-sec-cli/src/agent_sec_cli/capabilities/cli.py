"""Typer command for the agent capability configuration view."""

import typer
from agent_sec_cli.capabilities.view import (
    AGENTS,
    CANONICAL_CAPABILITIES,
    CapabilityViewError,
    query_capabilities,
    render_json,
    render_table,
)

app = typer.Typer(
    name="capabilities",
    help="Show agent-sec hook capabilities from the current CLI environment variables.",
    invoke_without_command=True,
)

_OUTPUT_FORMATS = {"table", "json"}


@app.callback(invoke_without_command=True)
def capabilities(
    ctx: typer.Context,
    agent: str | None = typer.Option(
        None,
        "--agent",
        "-a",
        help=f"Filter by agent. Allowed: {', '.join(AGENTS)}.",
    ),
    capability: str | None = typer.Option(
        None,
        "--capability",
        "-c",
        help=f"Filter by capability. Allowed: {', '.join(CANONICAL_CAPABILITIES)}.",
    ),
    output: str = typer.Option(
        "table",
        "--output",
        "-o",
        help="Output format: table or json.",
    ),
) -> None:
    """Show the capability view using this CLI process environment.

    This command reads only environment variables inherited by the CLI process.
    It does not read Agent config files, Agent home directories, or prove that
    hooks are loaded in the target Agent process.
    """
    if ctx.invoked_subcommand is not None:
        return
    if output not in _OUTPUT_FORMATS:
        typer.echo("Error: --output must be one of: json, table.", err=True)
        raise typer.Exit(code=1)
    try:
        records = query_capabilities(agent=agent, capability=capability)
    except CapabilityViewError as exc:
        typer.echo(f"Error: {exc}", err=True)
        raise typer.Exit(code=1) from exc
    if output == "json":
        typer.echo(render_json(records))
    else:
        typer.echo(render_table(records))
