# SWE-bench Runner

[![Python 3.12+](https://img.shields.io/badge/python-3.12+-blue.svg)](https://www.python.org/downloads/)

[Chinese](README_zh.md)

SWE-bench Runner is a local batch runner for SWE-bench. It loads SWE-bench
datasets, prepares isolated workspaces, invokes external coding agents, extracts
patches, writes SWE-bench-compatible `preds.json` files, and wraps the official
SWE-bench evaluation harness. It can also collect traces from OpenClaw local
session JSONL files and export CSV metrics.

The repository currently includes two built-in agent adapters:

- `cosh`: runs the local `cosh` CLI against Docker-backed SWE-bench workspaces.
- `openclaw`: runs OpenClaw local mode with one isolated profile and sandbox per
  SWE-bench instance.

## Features

- Run SWE-bench Lite, Verified, Full, Multilingual, or a custom HuggingFace dataset.
- Filter tasks by instance ID, regular expression, and slice.
- Run instances sequentially or with multiple workers.
- Skip already attempted instances by default, with `--redo` for forced reruns.
- Generate SWE-bench evaluation-compatible `preds.json`.
- Record per-instance results, run metadata, and input manifests.
- Evaluate patches with the official SWE-bench harness.
- Collect traces from OpenClaw local session JSONL files and export CSV reports.
- Load user-provided skill and per-case prompt resources from external
  directories.

## Requirements

Base requirements:

- Python 3.12+
- Docker, with the current user able to access the Docker daemon
- [uv](https://github.com/astral-sh/uv)
- Access to the HuggingFace datasets used by SWE-bench
- Access to SWE-bench evaluation Docker images, or permission to build them
  locally

Agent-specific requirements:

- `cosh`: the `cosh` executable must be available on `PATH`.
- `openclaw`: the `openclaw` executable must be available on `PATH`.

Quick environment checks:

```bash
docker info
which cosh
which openclaw
```

## Installation

Clone the repository and install runtime dependencies:

```bash
git clone <repository-url>
cd swebench-runner
uv sync
uv run swe-runner --help
```

For development dependencies:

```bash
uv sync --group dev
uv run --group dev pytest
```

Build a wheel:

```bash
uv build --wheel
```

## Quick Start

Run one SWE-bench Lite instance:

```bash
swe-runner run \
  --agent cosh \
  --subset lite \
  --split test \
  --instance-id django__django-11999
```

Run the first 10 Lite instances:

```bash
swe-runner run \
  --agent cosh \
  --subset lite \
  --slice 0:10
```

Run with multiple workers:

```bash
swe-runner run \
  --agent cosh \
  --subset lite \
  --workers 4
```

Evaluate generated patches:

```bash
swe-runner evaluate \
  --predictions ./output/run/preds.json \
  --subset lite
```

Collect and analyze OpenClaw traces from run metadata:

```bash
swe-runner analyze-traces \
  --run-metadata ./output/run/run_metadata.json
```

## Commands

```text
swe-runner
├── run             Run instances and generate patches
├── evaluate        Run the official SWE-bench evaluator
└── analyze-traces  Collect traces and export CSV reports
```

## `swe-runner run`

`run` is the main workflow command. It:

1. Checks Docker, disk space, and the selected agent executable.
2. Loads the dataset and applies filters.
3. Prepares one workspace per instance.
4. Invokes the selected agent adapter.
5. Extracts git diff from the workspace as `model_patch`.
6. Writes per-instance results, `preds.json`, input manifests, and run metadata.

### Datasets

`--subset` supports four built-in aliases:

| Alias | HuggingFace dataset |
|---|---|
| `lite` | `princeton-nlp/SWE-bench_Lite` |
| `verified` | `princeton-nlp/SWE-bench_Verified` |
| `full` | `princeton-nlp/SWE-bench` |
| `multilingual` | `SWE-bench/SWE-bench_Multilingual` |

You can also pass a custom HuggingFace dataset path:

```bash
swe-runner run \
  --agent cosh \
  --subset your-org/your-swe-dataset \
  --split test
```

Custom datasets must expose SWE-bench-compatible fields: `instance_id`, `repo`,
`version`, `base_commit`, `problem_statement`, `patch`, and `test_patch`.
`image_name` and `docker_image` are optional.

### Filtering

Filters are applied in this order:

1. `--instance-id`
2. `--filter`
3. `--slice`

Examples:

```bash
swe-runner run --agent cosh --instance-id django__django-11999
swe-runner run --agent cosh --filter "django__.*"
swe-runner run --agent cosh --slice 10:20
```

`--instance-id` accepts a comma-separated list:

```bash
swe-runner run \
  --agent cosh \
  --instance-id django__django-11999,matplotlib__matplotlib-18869
```

### Resume and Rerun

By default, runner checks `<output>/run/results/*.json` and skips instances that
already have result files. Use `--redo` to force a rerun:

```bash
swe-runner run --agent cosh --subset lite --redo
```

### Docker Registry

Use `--docker-pull-registry` to pull SWE-bench images from a custom registry.
Runner pulls from that registry first, then tags the image back to the original
name used by the rest of the workflow.

```bash
swe-runner run \
  --agent cosh \
  --subset lite \
  --docker-pull-registry registry.example.com
```

### Prompt Resource Directories

`--use-skill` and `--per-case-prompt` are mutually exclusive. Both options load
resources from user-provided directories.

#### Skill

With `--use-skill`, runner looks for:

```text
<skills-dir>/swe-bench-patch-generation/SKILL.md
```

Example:

```bash
swe-runner run \
  --agent openclaw \
  --subset lite \
  --use-skill \
  --skills-dir ./resources/skills
```

Behavior:

- `openclaw` merges the skill text into each instance workspace `AGENTS.md`.
- `cosh` adds a prompt instruction to use that skill.
- If the file does not exist, runner skips skill injection and continues.

#### Per-case Prompt

With `--per-case-prompt`, runner looks for one file per instance:

```text
<prompts-dir>/<instance_id>
```

Example:

```bash
swe-runner run \
  --agent openclaw \
  --subset lite \
  --per-case-prompt \
  --prompts-dir ./resources/prompts
```

Behavior:

- `openclaw` appends matched text to the instance user prompt inside
  `<task_guidance>`.
- `cosh` places matched text inside `<custom_instructions>`.
- If the file is missing or empty, runner skips per-case prompt guidance for
  that instance and continues.

### Tokenless

`--tokenless` is available only for agents that declare support for it. Among
the built-in adapters, only `openclaw` supports this option.

```bash
swe-runner run \
  --agent openclaw \
  --subset lite \
  --tokenless
```

When enabled, runner:

- Enables `plugins.entries.tokenless.enabled` in each OpenClaw per-case profile.
- Exposes the host tokenless OpenClaw extension to that profile.
- Finds and copies `rtk` and `tokenless` binaries into the instance workspace
  under `.runner/tokenless/bin/`.
- Prepends `/workspace/.runner/tokenless/bin` to `PATH` inside the sandbox.
- Writes tokenless evidence into run results so the configuration and runtime
  hook state can be inspected.

### Run Options

| Option | Default | Description |
|---|---:|---|
| `--agent, -a` | required | Agent name: `cosh` or `openclaw` |
| `--subset, -s` | `lite` | Dataset alias or HuggingFace dataset path |
| `--split` | `test` | Dataset split |
| `--output, -o` | `./output` | Output root; run artifacts go under `run/` |
| `--timeout` | `1200` | Agent timeout per instance, in seconds |
| `--step-limit` | `0` | Max steps; `0` means unlimited. Currently maps to a cosh CLI turn limit |
| `--slice` | none | Instance slice, for example `0:10`, `10:`, `:5` |
| `--filter` | none | Regex filter for instance IDs |
| `--instance-id, -i` | none | Instance ID, or comma-separated instance IDs |
| `--workers, -w` | `1` | Number of parallel workers |
| `--docker-pull-registry` | none | Registry host used as the Docker pull source |
| `--use-skill` | `false` | Load skill resources from `--skills-dir` |
| `--skills-dir` | none | Skill resource root directory |
| `--per-case-prompt` | `false` | Load per-instance prompt files from `--prompts-dir` |
| `--prompts-dir` | none | Per-instance prompt resource root directory |
| `--tokenless` | `false` | Enable tokenless/rtk injection; currently supported by `openclaw` |
| `--redo` | `false` | Rerun instances that already have result files |
| `--verbose, -v` | `false` | Write DEBUG logs |

## Agent Adapters

### `cosh`

The `cosh` adapter starts a SWE-bench Docker workspace for each instance and
builds a prompt containing the problem statement, workspace path, container
name, and execution rules. The command shape is:

```text
cosh --yolo <prompt>
```

If `--step-limit N` is set, runner also passes:

```text
--max-session-turns N
```

After the agent exits, runner extracts git diff from the workspace and writes
it into `preds.json`.

### `openclaw`

The `openclaw` adapter uses OpenClaw local mode. Each instance gets an isolated
OpenClaw profile, session, and sandbox agent.

Main behavior:

- Prepares the SWE-bench repository in a temporary workspace.
- Mounts the repository into the OpenClaw sandbox at `/testbed`.
- Uses a separate OpenClaw workspace mounted at `/workspace`.
- Writes per-instance profiles under `<output>/run/openclaw-profiles/<instance_id>/`.
- Copies the base OpenClaw config and mutates only the per-instance profile.
- Runs through `openclaw --profile <profile> agent --local --json ...`.
- Cleans profile symlinks, temporary workspaces, and sandbox containers after
  execution.

Base OpenClaw config resolution order:

1. Adapter-provided config path
2. `OPENCLAW_CONFIG_PATH`
3. `~/.openclaw/openclaw.json`

If the base config file does not exist, runner writes an empty config object for
the instance profile and then adds the required sandbox config.

## `swe-runner evaluate`

`evaluate` wraps the official SWE-bench evaluation flow. It reads `preds.json`
and evaluates only instances with non-empty `model_patch` values.

Default evaluation:

```bash
swe-runner evaluate
```

Specify predictions and output:

```bash
swe-runner evaluate \
  --predictions ./output/run/preds.json \
  --output ./output \
  --subset lite \
  --workers 4
```

`evaluate --subset` accepts only the built-in aliases: `lite`, `verified`,
`full`, and `multilingual`.

To build evaluation images locally:

```bash
swe-runner evaluate --namespace none
```

### Evaluate Options

| Option | Default | Description |
|---|---:|---|
| `--predictions, -p` | `./output/run/preds.json` | Predictions file |
| `--subset, -s` | `lite` | Evaluation dataset: `lite`, `verified`, `full`, or `multilingual` |
| `--split` | `test` | Dataset split |
| `--output, -o` | `./output` | Output root; evaluation artifacts go under `evaluate/` |
| `--workers, -w` | `4` | SWE-bench evaluation workers |
| `--timeout` | `1800` | Evaluation timeout per instance, in seconds |
| `--run-id` | auto-generated | SWE-bench evaluation run ID |
| `--cache-level` | `env` | Docker cache level: `none`, `base`, `env`, `instance` |
| `--namespace` | `swebench` | Evaluation image namespace; `none` builds locally |
| `--verbose, -v` | `false` | Write DEBUG logs |

## `swe-runner analyze-traces`

`analyze-traces` reads trace JSON files and exports per-trace details,
per-case summaries, and detailed metric CSVs. It can also use `run_metadata.json`
and OpenClaw profiles to collect traces from session JSONL files.

Analyze existing traces:

```bash
swe-runner analyze-traces \
  --trace-root ./traces \
  --output ./output
```

Infer the trace window and OpenClaw profiles from run metadata:

```bash
swe-runner analyze-traces \
  --run-metadata ./output/run/run_metadata.json \
  --output ./output
```

Specify OpenClaw profiles explicitly:

```bash
swe-runner analyze-traces \
  --run-metadata ./output/run/run_metadata.json \
  --openclaw-profiles-dir ./output/run/openclaw-profiles
```

### Analyze Options

| Option | Default | Description |
|---|---:|---|
| `--trace-root` | `<output>/analyze-traces/traces` | Trace JSON root directory |
| `--output, -o` | `./output` | Output root |
| `--trim-ratio` | `0.1` | Tail trim ratio, in `[0, 0.5)` |
| `--openclaw-profiles-dir` | inferred from `--run-metadata` | OpenClaw local profiles directory |
| `--start` | none | Trace collection window start, ISO-8601 or epoch |
| `--end` | `now` | Trace collection window end |
| `--run-metadata` | none | `run_metadata.json` produced by `run` |

## Output Layout

The default output root is `./output`. Each command writes to its own subdir:

```text
output/
├── run/
│   ├── preds.json
│   ├── run_metadata.json
│   ├── swe-runner.run.log
│   ├── results/
│   │   └── <instance_id>.json
│   ├── input-manifests/
│   │   └── <instance_id>/input_manifest.json
│   ├── openclaw-profiles/
│   │   └── <instance_id>/
│   ├── openclaw-errors/
│   │   └── <instance_id>.log
│   └── openclaw-tokenless-evidence/
│       └── <instance_id>.json
├── evaluate/
│   ├── swe-runner.evaluate.log
│   ├── <model>.<run_id>.json
│   └── logs/run_evaluation/<run_id>/<model>/<instance_id>/
└── analyze-traces/
    ├── swe-runner.analyze-traces.log
    ├── traces/
    │   └── <instance_id>/trace*.json
    ├── trace_details/
    │   └── <instance_id>.csv
    ├── trace_metrics/
    │   └── trace_metrics.csv
    └── trace_summary.csv
```

Important files:

| File | Description |
|---|---|
| `run/preds.json` | Predictions file consumable by SWE-bench evaluation |
| `run/results/*.json` | Per-instance run summaries, written for successes and failures |
| `run/run_metadata.json` | Run timing, agent, worker, instance, and metadata mapping information |
| `run/input-manifests/*/input_manifest.json` | Per-instance input snapshot with dataset row, settings, prompt hash, resource records, and runner info |
| `run/openclaw-profiles/` | OpenClaw local per-instance profiles and session artifacts |
| `run/openclaw-errors/*.log` | stdout/stderr for non-zero OpenClaw local exits |
| `run/openclaw-tokenless-evidence/*.json` | Configuration and runtime evidence for `--tokenless` runs |
| `evaluate/*.json` | SWE-bench evaluation summary report |
| `analyze-traces/trace_details/*.csv` | Per-trace basic metrics |
| `analyze-traces/trace_summary.csv` | Per-instance summary metrics |
| `analyze-traces/trace_metrics/trace_metrics.csv` | Detailed trace, tool-call, and token metrics |

## Project Structure

```text
src/swe_runner/
├── cli.py                  # Typer CLI entry point
├── cli_commands.py         # CLI command handlers and settings assembly
├── agents/                 # Agent adapter abstraction, registry, and built-in adapters
│   ├── cosh/
│   └── openclaw/
├── common/                 # Pydantic models, logging, and dataset aliases
├── run/                    # run workflow
│   ├── dataset.py          # Dataset loading and filtering
│   ├── session.py          # run session entry point
│   ├── execution/          # Concurrent scheduling and single-instance lifecycle
│   ├── io/                 # results, metadata, manifests, and report output
│   ├── prompting/          # Prompt and external resource loading
│   └── workspace/          # Docker, git, patch, and workspace rules
├── evaluation/             # SWE-bench evaluation wrapper
└── trace_extraction/       # OpenClaw trace collection, reconstruction, analysis, and export
```

## Development

Run tests:

```bash
uv run --group dev pytest
uv run --group dev pytest tests/unit/cli/test_cli.py
```

Static checks:

```bash
uv run --group dev ruff check src tests
uv run --group dev mypy src/swe_runner
```

Format:

```bash
uv run --group dev ruff format src tests
```

## Troubleshooting

### Docker Is Unavailable

```bash
docker info
```

Make sure the Docker daemon is running and the current user can access it.

### Low Disk Space

SWE-bench images, containers, and workspaces can use substantial disk space.
Check available space:

```bash
df -h
```

Clean Docker cache:

```bash
docker system prune -a
```

### Agent Executable Not Found

```bash
which cosh
which openclaw
```

Make sure the selected agent CLI is installed and visible on the current
shell's `PATH`.

### HuggingFace or Docker Pulls Are Slow

Configure HuggingFace caching, Docker registry mirrors, or use
`--docker-pull-registry` to select the image pull source.

### OpenClaw Local Fails

Check:

- `which openclaw`
- `OPENCLAW_CONFIG_PATH`
- `~/.openclaw/openclaw.json`
- `output/run/openclaw-profiles/<instance_id>/openclaw.json`
- `output/run/openclaw-errors/<instance_id>.log`

The OpenClaw adapter does not mutate the base config file. It copies the base
config and modifies only the per-instance profile config.

## License

This project is licensed under the Apache License 2.0. See [LICENSE](LICENSE).

## Acknowledgements

- [SWE-bench](https://www.swebench.com/)
- [SWE-bench official evaluation harness](https://github.com/SWE-bench/SWE-bench)
