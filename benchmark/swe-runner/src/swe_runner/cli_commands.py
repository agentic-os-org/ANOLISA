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

"""Command handlers behind the Typer CLI."""

from __future__ import annotations

import datetime
import logging
from dataclasses import dataclass
from pathlib import Path

from swe_runner.agents import agent_supports_run_option
from swe_runner.common.logging_config import setup_logging
from swe_runner.common.models import AgentConfig, DatasetConfig, OutputConfig, Settings
from swe_runner.evaluation import run_evaluation as run_patch_evaluation
from swe_runner.run.io.report import RunReport
from swe_runner.run.session import RunSession
from swe_runner.trace_extraction import TraceCollectionPlan, write_trace_analysis_csvs

logger = logging.getLogger(__name__)

RUN_OUTPUT_SUBDIR = "run"
EVALUATE_OUTPUT_SUBDIR = "evaluate"
ANALYZE_TRACES_OUTPUT_SUBDIR = "analyze-traces"
TOKENLESS_RUN_OPTION = "tokenless"


class CommandUsageError(ValueError):
    """Raised when CLI arguments are valid Typer values but invalid together."""


@dataclass(frozen=True)
class TraceAnalysisCommandResult:
    """Files produced by the trace analysis command."""

    recorded_trace_count: int | None
    detail_dir: Path
    summary_csv: Path
    trace_metrics_csv: Path


def command_output_dir(output_root: Path, command_name: str) -> Path:
    """Return the per-command output directory under an output root."""
    return output_root / command_name


def run_instances_command(
    *,
    agent: str,
    subset: str,
    split: str,
    output: Path,
    timeout: int,
    step_limit: int,
    slice_range: str | None,
    filter_regex: str | None,
    instance_id: str | None,
    workers: int,
    docker_pull_registry: str | None,
    use_skill: bool,
    skills_dir: Path | None = None,
    tokenless: bool,
    per_case_prompt: bool,
    prompts_dir: Path | None = None,
    redo: bool,
    verbose: bool,
) -> RunReport:
    """Build run settings, execute a run session, and return its report."""
    settings = build_run_settings(
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
    )
    setup_logging(settings.output.output_dir, verbose=verbose, suffix=RUN_OUTPUT_SUBDIR)
    return RunSession(settings, redo=redo).execute()


def build_run_settings(
    *,
    agent: str,
    subset: str,
    split: str,
    output: Path,
    timeout: int,
    step_limit: int,
    slice_range: str | None,
    filter_regex: str | None,
    instance_id: str | None,
    workers: int,
    docker_pull_registry: str | None,
    use_skill: bool,
    skills_dir: Path | None = None,
    tokenless: bool,
    per_case_prompt: bool,
    prompts_dir: Path | None = None,
) -> Settings:
    """Build validated run settings from CLI values."""
    if use_skill and per_case_prompt:
        raise CommandUsageError("--use-skill and --per-case-prompt are mutually exclusive")
    if tokenless and not agent_supports_run_option(agent, TOKENLESS_RUN_OPTION):
        raise CommandUsageError(f"--tokenless is not supported by agent '{agent}'")

    instance_ids = [item.strip() for item in instance_id.split(",")] if instance_id else None
    return Settings(
        agent=AgentConfig(
            name=agent,
            timeout=timeout,
            step_limit=step_limit,
            workers=workers,
            docker_pull_registry=docker_pull_registry,
            use_skill=use_skill,
            skills_dir=skills_dir,
            tokenless=tokenless,
            per_case_prompt=per_case_prompt,
            prompts_dir=prompts_dir,
        ),
        dataset=DatasetConfig(
            subset=subset,
            split=split,
            filter_regex=filter_regex,
            slice_range=slice_range,
            instance_ids=instance_ids,
        ),
        output=OutputConfig(output_dir=command_output_dir(output, RUN_OUTPUT_SUBDIR)),
    )


def evaluate_patches_command(
    *,
    predictions: Path,
    subset: str,
    split: str,
    output: Path,
    workers: int,
    timeout: int,
    run_id: str | None,
    cache_level: str,
    namespace: str,
    verbose: bool,
) -> None:
    """Evaluate generated patches with the SWE-bench evaluator."""
    if not predictions.exists():
        raise CommandUsageError(f"Predictions file not found: {predictions}")

    resolved_predictions = predictions.resolve()
    actual_run_id = run_id or f"eval-{datetime.datetime.now().strftime('%Y%m%d-%H%M%S')}"
    evaluate_output = command_output_dir(output, EVALUATE_OUTPUT_SUBDIR)
    setup_logging(evaluate_output, verbose=verbose, suffix=EVALUATE_OUTPUT_SUBDIR)

    logger.info(
        "EVALUATE_START predictions=%s subset=%s split=%s output_root=%s output=%s workers=%s run_id=%s",
        resolved_predictions,
        subset,
        split,
        output,
        evaluate_output,
        workers,
        actual_run_id,
    )
    run_patch_evaluation(
        resolved_predictions,
        evaluate_output,
        subset=subset,
        split=split,
        workers=workers,
        timeout=timeout,
        run_id=actual_run_id,
        cache_level=cache_level,
        namespace=None if namespace.lower() == "none" else namespace,
    )


def analyze_traces_command(
    *,
    trace_root: Path | None,
    output: Path,
    trim_ratio: float,
    openclaw_profiles_dir: Path | None,
    start: str | None,
    end: str,
    run_metadata: Path | None,
) -> TraceAnalysisCommandResult:
    """Collect available traces and export analysis CSV files."""
    analyze_output = command_output_dir(output, ANALYZE_TRACES_OUTPUT_SUBDIR)
    effective_trace_root = trace_root or analyze_output / "traces"
    setup_logging(analyze_output, suffix=ANALYZE_TRACES_OUTPUT_SUBDIR)

    plan = TraceCollectionPlan.resolve(
        start=start,
        end=end,
        run_metadata_path=run_metadata,
        openclaw_profiles_dir=openclaw_profiles_dir,
    )
    trace_files = plan.collect(effective_trace_root)
    detail_dir, summary_csv = write_trace_analysis_csvs(
        trace_root=effective_trace_root,
        output_dir=analyze_output,
        trim_ratio=trim_ratio,
    )
    return TraceAnalysisCommandResult(
        recorded_trace_count=len(trace_files) if trace_files is not None else None,
        detail_dir=detail_dir,
        summary_csv=summary_csv,
        trace_metrics_csv=analyze_output / "trace_metrics" / "trace_metrics.csv",
    )
