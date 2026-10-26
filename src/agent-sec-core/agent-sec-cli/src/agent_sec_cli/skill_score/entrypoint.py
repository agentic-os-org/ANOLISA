"""Dedicated assessment CLI without changing the system AgentSecCore executable."""

import typer

from agent_sec_cli.skill_score.cli import app as score_app

app = typer.Typer(no_args_is_help=True)
app.add_typer(score_app, name="skill-score")


def main() -> None:
    """Run the separately installed assessment command group."""
    app()


if __name__ == "__main__":
    main()
