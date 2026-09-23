# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""CLI entry point for swe-runner."""

from pathlib import Path

import typer
from rich.console import Console

from swe_runner.agents import AgentEnvironmentError, list_available_agent_names
from swe_runner.cli_commands import (
    CommandUsageError,
    analyze_traces_command,
    evaluate_patches_command,
    run_instances_command,
)
from swe_runner.trace_extraction import ExtractionError

app = typer.Typer(
    name="swe-runner",
    help="Unified SWE-bench runner with pluggable agents",
)
console = Console()


def _agent_help() -> str:
    return f"Agent name. Available: {', '.join(list_available_agent_names())}"


@app.callback()
def main() -> None:
    pass


@app.command()
def run(
    agent: str = typer.Option(..., "--agent", "-a", help=_agent_help()),
    subset: str = typer.Option(
        "lite",
        "--subset",
        "-s",
        help="Dataset subset (lite/verified/full/multilingual or custom path)",
    ),
    split: str = typer.Option("test", "--split", help="Dataset split"),
    output: Path = typer.Option("./output", "--output", "-o", help="Output root directory"),
    timeout: int = typer.Option(1200, "--timeout", help="Agent timeout in seconds"),
    step_limit: int = typer.Option(0, "--step-limit", help="Max agent steps (0=unlimited)"),
    slice_range: str | None = typer.Option(None, "--slice", help="Instance slice (e.g., 0:5)"),
    filter_regex: str | None = typer.Option(None, "--filter", help="Regex filter for instance IDs"),
    instance_id: str | None = typer.Option(
        None, "--instance-id", "-i", help="Instance ID(s), comma-separated for multiple"
    ),
    workers: int = typer.Option(1, "--workers", "-w", help="Number of parallel workers (default: 1)"),
    docker_pull_registry: str | None = typer.Option(
        None,
        "--docker-pull-registry",
        help="Registry host to use as the source for docker pull.",
    ),
    use_skill: bool = typer.Option(
        False,
        "--use-skill",
        help="Enable SWE-bench skill guidance from --skills-dir; mutually exclusive with --per-case-prompt",
    ),
    skills_dir: Path | None = typer.Option(
        None,
        "--skills-dir",
        help="Directory containing skills as <skill-name>/SKILL.md",
    ),
    tokenless: bool = typer.Option(
        False,
        "--tokenless",
        help="Enable tokenless/rtk helper injection for agents that support it",
    ),
    per_case_prompt: bool = typer.Option(
        False,
        "--per-case-prompt",
        help="Enable per-instance prompt guidance from --prompts-dir; mutually exclusive with --use-skill",
    ),
    prompts_dir: Path | None = typer.Option(
        None,
        "--prompts-dir",
        help="Directory containing per-instance prompt files named by instance_id",
    ),
    redo: bool = typer.Option(False, "--redo", help="Re-run already completed instances"),
    verbose: bool = typer.Option(False, "--verbose", "-v", help="Verbose output"),
) -> None:
    """Run SWE-bench evaluation with the specified agent."""
    try:
        report = run_instances_command(
            agent=agent,
            subset=subset,
            split=split,
            output=output,
            timeout=timeout,
            step_limit=step_limit,
            slice_range=slice_range,
            filter_regex=filter_regex,
            instance_id=instance_id,
            workers=workers,
            docker_pull_registry=docker_pull_registry,
            use_skill=use_skill,
            skills_dir=skills_dir,
            tokenless=tokenless,
            per_case_prompt=per_case_prompt,
            prompts_dir=prompts_dir,
            redo=redo,
            verbose=verbose,
        )
    except CommandUsageError as e:
        console.print(f"[red]Error:[/red] {e}")
        raise typer.Exit(code=1) from None
    except AgentEnvironmentError as e:
        console.print(f"[red]Environment check failed:[/red] {e}")
        raise typer.Exit(code=1) from None
    except KeyError as e:
        console.print(f"[red]Error:[/red] {e}")
        raise typer.Exit(code=1) from None

    if report.total == 0:
        console.print("[yellow]No instances to process.[/yellow]")
        raise typer.Exit(code=0)

    console.print(f"\n[green]Done:[/green] {report.succeeded}/{report.total} succeeded")
    console.print(f"[green]Run metadata:[/green] {report.metadata_path}")


@app.command()
def evaluate(
    predictions: Path = typer.Option("./output/run/preds.json", "--predictions", "-p", help="Path to preds.json file"),
    subset: str = typer.Option("lite", "--subset", "-s", help="Dataset subset (lite/verified/full/multilingual)"),
    split: str = typer.Option("test", "--split", help="Dataset split"),
    output: Path = typer.Option("./output", "--output", "-o", help="Output root directory"),
    workers: int = typer.Option(4, "--workers", "-w", help="Number of parallel workers"),
    timeout: int = typer.Option(1800, "--timeout", help="Evaluation timeout in seconds per instance"),
    run_id: str | None = typer.Option(None, "--run-id", help="Unique run identifier (auto-generated if not set)"),
    cache_level: str = typer.Option("env", "--cache-level", help="Docker image cache level (none/base/env/instance)"),
    namespace: str = typer.Option(
        "swebench",
        "--namespace",
        help="Image namespace for evaluation instances; use 'swebench' for official prebuilt images or 'none' to build locally",
    ),
    verbose: bool = typer.Option(False, "--verbose", "-v", help="Verbose output"),
) -> None:
    """Evaluate generated patches using SWE-bench standard evaluation."""
    try:
        evaluate_patches_command(
            predictions=predictions,
            subset=subset,
            split=split,
            output=output,
            workers=workers,
            timeout=timeout,
            run_id=run_id,
            cache_level=cache_level,
            namespace=namespace,
            verbose=verbose,
        )
    except CommandUsageError as e:
        console.print(f"[red]Error:[/red] {e}")
        raise typer.Exit(code=1) from None


@app.command("analyze-traces")
def analyze_traces(
    trace_root: Path | None = typer.Option(
        None,
        "--trace-root",
        help="Trace JSON root directory (default: <output>/analyze-traces/traces)",
    ),
    output: Path = typer.Option(Path("./output"), "--output", "-o", help="Output root directory"),
    trim_ratio: float = typer.Option(0.1, "--trim-ratio", help="Tail trim ratio for trimmed means"),
    openclaw_profiles_dir: Path | None = typer.Option(
        None,
        "--openclaw-profiles-dir",
        help="OpenClaw local profiles directory containing <profile>/agents/<agent>/sessions/*.jsonl",
    ),
    start: str | None = typer.Option(None, "--start", help="Trace window start timestamp (ISO-8601 or epoch)"),
    end: str = typer.Option("now", "--end", help="Trace window end timestamp (default: now)"),
    run_metadata: Path | None = typer.Option(None, "--run-metadata", help="Path to run_metadata.json from run command"),
) -> None:
    """Analyze recorded trace JSON files and export CSV summaries."""
    try:
        result = analyze_traces_command(
            trace_root=trace_root,
            output=output,
            trim_ratio=trim_ratio,
            openclaw_profiles_dir=openclaw_profiles_dir,
            start=start,
            end=end,
            run_metadata=run_metadata,
        )
    except ExtractionError as e:
        console.print(f"[red]Error:[/red] {e}")
        raise typer.Exit(code=1) from None

    if result.recorded_trace_count is not None:
        console.print(f"[green]Recorded traces:[/green] {result.recorded_trace_count}")
    console.print(f"[green]Per-trace CSV dir:[/green] {result.detail_dir}")
    console.print(f"[green]Per-case summary CSV:[/green] {result.summary_csv}")
    console.print(f"[green]Trace metrics CSV:[/green] {result.trace_metrics_csv}")


if __name__ == "__main__":
    app()
