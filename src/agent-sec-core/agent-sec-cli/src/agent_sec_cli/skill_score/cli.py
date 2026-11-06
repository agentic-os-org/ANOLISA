"""Minimal JSON-only plan/run commands; detailed evidence is explicitly opt-in."""

import contextlib
import json
import os
import signal
import sys
import tempfile
from pathlib import Path
from types import FrameType
from typing import Annotated, Any

import typer
from agent_sec_cli.skill_score.config import ScoreConfig
from agent_sec_cli.skill_score.contracts import ContractError
from agent_sec_cli.skill_score.evidence.redact import redact_text, redact_value
from agent_sec_cli.skill_score.io import read, scrub_configured_key
from agent_sec_cli.skill_score.pipeline import plan_skill, run_task
from agent_sec_cli.skill_score.task import read_task_input

app = typer.Typer(
    help="Plan Skill tasks, then execute and score their safety.", no_args_is_help=True
)


def check_targets(
    skill: Path, output_file: Path | None, evidence_dir: Path | None
) -> None:
    for path in (output_file, evidence_dir):
        if path is not None and path.resolve().is_relative_to(skill.resolve()):
            raise ContractError("outputs must not be inside the evaluated Skill")
    if output_file is not None and os.path.lexists(output_file):
        raise ContractError("output file already exists")
    if evidence_dir is not None and os.path.lexists(evidence_dir):
        if (
            evidence_dir.is_symlink()
            or not evidence_dir.is_dir()
            or any(evidence_dir.iterdir())
        ):
            raise ContractError(
                "evidence directory must be absent or empty, and not a symlink"
            )
    if output_file is not None and evidence_dir is not None:
        if output_file.resolve() == evidence_dir.resolve():
            raise ContractError(
                "output file and evidence directory must be different paths"
            )
        if output_file.resolve().is_relative_to(evidence_dir.resolve()):
            first = output_file.resolve().relative_to(evidence_dir.resolve()).parts[0]
            if first.split(".")[0] in {
                "probes",
                "task",
                "environment",
                "runtime-signals",
                "evidence-catalog",
                "judge-catalog",
                "adjudication",
                "critical",
                "internal-report",
                "events",
            }:
                raise ContractError(
                    "output file conflicts with reserved internal evidence paths"
                )
    for directory in ([output_file.parent] if output_file is not None else []) + (
        [evidence_dir] if evidence_dir is not None else []
    ):
        directory.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryFile(dir=directory):
            pass


def interrupted(_signal: int, _frame: FrameType | None) -> None:
    raise KeyboardInterrupt


def emit(result: dict[str, Any] | None, output_file: Path | None, code: int) -> None:
    if result is not None:
        serialized = (
            json.dumps(redact_value(result), ensure_ascii=False, indent=2) + "\n"
        )
        if output_file is not None:
            try:
                with output_file.open("x", encoding="utf-8") as stream:
                    stream.write(serialized)
            except OSError as exc:
                typer.echo("cannot save result: " + redact_text(str(exc)), err=True)
                code = 1
        typer.echo(serialized, nl=False)
    if code:
        raise typer.Exit(code)


def execute(
    stage: str,
    argument: str,
    output_file: Path | None,
    evidence_dir: Path | None,
) -> None:
    task = None
    try:
        if stage == "plan":
            skill = Path(argument).resolve()
            if not (skill / "SKILL.md").is_file():
                raise ContractError("skill-dir must contain a readable SKILL.md")
        else:
            task = read_task_input(
                argument, sys.stdin.read() if argument == "-" else ""
            )
            skill = Path(task.skill_path)
            if not (skill / "SKILL.md").is_file():
                raise ContractError("the original Skill directory is unavailable")
        check_targets(skill, output_file, evidence_dir)
    except (OSError, ValueError) as exc:
        typer.echo("invalid input: " + redact_text(str(exc)), err=True)
        raise typer.Exit(2) from None
    result, code = None, 0
    temporary = None
    old_handler = signal.signal(signal.SIGTERM, interrupted)
    try:
        if evidence_dir is None:
            temporary = tempfile.TemporaryDirectory(prefix="agent-sec-skill-score-")
            directory = Path(temporary.name)
        else:
            directory = evidence_dir.resolve()
        with contextlib.redirect_stdout(sys.stderr):
            if stage == "plan":
                result = plan_skill(skill, directory, ScoreConfig())
            else:
                result = run_task(task, directory, ScoreConfig())
                failures = read(directory / "internal-report.json")["failures"]
                if failures or result["decision"] == "inconclusive":
                    code = 1
                for failure in failures:
                    typer.echo(redact_text(failure), err=True)
                for limitation in result.get("limitations", []):
                    typer.echo(
                        "limited assessment: " + redact_text(limitation), err=True
                    )
                if evidence_dir is not None:
                    result["evidence_dir"] = str(directory)
    except KeyboardInterrupt:
        typer.echo("skill-score interrupted", err=True)
        code = 1
    except (OSError, ValueError) as exc:
        typer.echo("skill-score failed: " + redact_text(str(exc)), err=True)
        code = 2 if isinstance(exc, ContractError) and stage == "run" else 1
    except Exception as exc:
        typer.echo(
            "skill-score failed: " + redact_text(f"{type(exc).__name__}: {exc}"),
            err=True,
        )
        code = 1
    finally:
        try:
            if temporary is not None:
                temporary.cleanup()
            elif evidence_dir is not None:
                scrub_configured_key(evidence_dir)
        except OSError as exc:
            typer.echo("evidence cleanup failed: " + redact_text(str(exc)), err=True)
            code = 1
        signal.signal(signal.SIGTERM, old_handler)
    emit(result, output_file, code)


@app.command("plan")
def plan(
    skill_dir: Annotated[str, typer.Argument(help="One local Skill directory.")],
    output_file: Annotated[Path | None, typer.Option("--output-file")] = None,
) -> None:
    execute("plan", skill_dir, output_file, None)


@app.command("run")
def run(
    task_input: Annotated[
        str, typer.Argument(help="Task JSON file, inline JSON, or - for stdin.")
    ],
    output_file: Annotated[Path | None, typer.Option("--output-file")] = None,
    evidence_dir: Annotated[Path | None, typer.Option("--evidence-dir")] = None,
) -> None:
    execute("run", task_input, output_file, evidence_dir)


if __name__ == "__main__":
    app()
