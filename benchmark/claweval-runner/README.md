# ce-runner

[中文版](README_zh.md)

Orchestrate [claw-eval](https://github.com/claw-eval/claw-eval) evaluation tasks through the openclaw agent. Handles agent execution, session-to-trace conversion, and LLM-judge grading.

## Quick Start

```bash
# 1. Clone the anolisa monorepo and enter this runner
git clone <anolisa-repo-url>
cd anolisa/benchmark/claweval-runner

# 2. One-shot environment setup (see below for what it does).
#    This also clones the pinned claw-eval sources into ./claw-eval.
MODEL_API_KEY=sk-xxx python scripts/setup_env.py

# 3. Run via `uv run` (uses the .venv created by setup, no activation needed).
#    Pass --config so the run picks up claw-eval/config.yaml (not auto-loaded).
uv run ce-runner run claw-eval/tasks/T001zh_email_triage --config claw-eval/config.yaml
uv run ce-runner batch --prefix T --range 1-5 --parallel 4 --config claw-eval/config.yaml
```

> **Always invoke commands with `uv run <cmd>`.** It auto-resolves the project's `.venv`, ensures the right interpreter / dependencies, and avoids stale shell PATH issues. `source .venv/bin/activate` is supported but discouraged.

## Prerequisites

These must be installed **before** running `setup_env.py`:

- Docker >= 24.0 (daemon running)
- Node.js >= 18 (for the `openclaw` CLI)
- openclaw **= 2026.4.22** (`openclaw --version`) — **Only version 4.22 has been fully tested; other versions may have compatibility issues. Strongly recommend using 4.22.**
- Python >= 3.11

`MODEL_API_KEY` should be exported (optionally `MODEL_BASE_URL`, `MODEL_ID`); otherwise the model-config step is skipped.

## Environment Setup

`scripts/setup_env.py` is idempotent and end-to-end. It runs the following steps:

| Step | Action |
|------|--------|
| 1 | Check prerequisites: Python >= 3.11, git, curl, Docker, Node.js >= 18, npm, openclaw = 2026.4.22 |
| 2 | Clone the pinned `claw-eval` sources into `claw-eval/` (repo + revision pinned in `scripts/setup_env.py`) |
| 3 | Ensure `uv` package manager (standalone binary install via curl) |
| 4 | Create `./.venv` via `uv venv`; inject `VIRTUAL_ENV` + `PATH` |
| 5 | Install ce-runner `[dev]` + claw-eval `[mock,sandbox]` + sandbox-server reqs via `uv pip` |
| 6 | Run `scripts/configure_openclaw.py` to set openclaw config (with pre/post gateway health check) |
| 7 | Ensure openclaw gateway service: `install` if missing/disabled, `start` if stopped |
| 8 | Run `scripts/configure_model.py --api-key $MODEL_API_KEY` (skipped if env unset) |
| 9 | Install `mcporter` via npm (required for MCP tool dispatch) |
| 10 | Patch `claw-eval/Dockerfile.agent` — backup to `.bak` and remove TUNA mirror lines |
| 11 | `docker build -t claw-eval-agent:latest -f claw-eval/Dockerfile.agent claw-eval/` |
| 12 | Download + extract task fixtures from Hugging Face (videos, etc.; `--skip-fixtures` to skip) |
| 13 | Verify: `ce-runner --help`, `claw-eval --help`, `openclaw gateway status`, Docker, env cleanliness |

After setup, prefix every command with `uv run` (e.g. `uv run ce-runner --help`, `uv run pytest`, `uv run python scripts/list_tasks.py`). `ce-runner` and `claw-eval` are exposed as console scripts inside the venv and resolved automatically by `uv run`.

## Model Configuration

```bash
# Re-run with a new key any time
uv run python scripts/configure_model.py --api-key sk-xxx \
  --base-url https://api.provider.com/v1 --model-id your-model

# Or via environment
export MODEL_API_KEY=sk-xxx MODEL_BASE_URL=https://api.example.com/v1 MODEL_ID=gpt-4o
```

## Usage

All commands below assume `uv run` (project venv is auto-resolved).

> **`--config` is required unless the model/judge env vars are exported.** `run`/`batch` do **not** auto-load `claw-eval/config.yaml`; pass `--config claw-eval/config.yaml` (written by `scripts/configure_model.py`), or export `MODEL_API_KEY`/`MODEL_BASE_URL`/`MODEL_ID` and `JUDGE_API_KEY`/`JUDGE_BASE_URL`/`JUDGE_MODEL_ID`.

```bash
# Single task
uv run ce-runner run claw-eval/tasks/T001zh_email_triage --config claw-eval/config.yaml

# Batch
uv run ce-runner batch --prefix T --range 1-10 --parallel 5 --config claw-eval/config.yaml
uv run ce-runner batch --prefix T --range 1-5 --trials 3 --parallel 5 --config claw-eval/config.yaml
uv run ce-runner batch --tag general --parallel 4 --config claw-eval/config.yaml
uv run ce-runner batch --tasks-file tasks.txt --parallel 4 --config claw-eval/config.yaml
uv run ce-runner batch --tasks-string T001zh_email_triage,T002zh_calendar --parallel 4 --config claw-eval/config.yaml
uv run ce-runner batch --prefix M --range 1-10:2 --parallel 3 --config claw-eval/config.yaml   # every 2nd M task
```

### Task Types

ce-runner supports three task prefixes with different execution modes:

| Prefix | Mode | Description |
|--------|------|-------------|
| T | General | Mock services + sandbox container; standard tool-use evaluation |
| M | Multimodal | HTTP API first turn (image/video attachments) + sandbox container |
| C | User Agent | Multi-round agent ↔ UserAgent dialogue loop |

### Key Options

| Option | Default | Description |
|---|---|---|
| `--config` | — | Path to config YAML (e.g. `claw-eval/config.yaml`) with `model`/`judge` sections. Required unless the model/judge env vars are exported. |
| `--parallel N` | 4 | Number of parallel agent workers |
| `--trials N` | 1 | Trials per task (for pass@k) |
| `--timeout N` | 600 | Agent timeout in seconds |
| `--sandbox-image` | `claw-eval-agent:latest` | Docker image for sandbox |
| `--tasks-file` | — | File with task IDs (one per line) |
| `--tasks-string` | — | Comma-separated exact task names (mutually exclusive with --tasks-file) |
| `--filter` | — | Substring match on task directory name |
| `--grade-parallel` | min(parallel,2) | Parallel workers for LLM judge grading |
| `--chunk-size` | 4 | Tasks per chunk (controls peak memory; auto-raised to --parallel) |
| `--trace-prefix` | `openclaw` | Prefix for trace directory name |
| `--skip-preflight` | false | Skip openclaw plugins + docker pre-flight checks |

## Output

```
claw-eval/traces/openclaw_<YY-MM-DD-HH-MM>/
├── <task_id>_xxxx.jsonl   # Converted trace
├── batch_results.json     # Per-trial results
└── batch_summary.json     # Aggregate summary
```

## Architecture: MCP Tool Injection & Anti-Cheat Isolation

ce-runner uses openclaw's native MCP runtime (stdio) to expose task-specific tools to the agent. The key challenge is ensuring the agent can **only** access its own task's tools and cannot read host files (e.g. `grader.py`) to cheat.

### Core Mechanism

Each task agent is configured with three tool policies working together:

```json
{
  "id": "claweval-t001zh_email_triage",
  "tools": {
    "allow": [
      "exec",
      "claw-eval-mock-T001zh_email_triage__gmail_list_messages",
      "claw-eval-mock-T001zh_email_triage__gmail_send_message",
      "claw-eval-sandbox-T001zh_email_triage__Bash",
      "claw-eval-sandbox-T001zh_email_triage__Read",
      "..."
    ],
    "deny": [
      "exec", "read", "write", "edit", "process", "browser",
      "canvas", "nodes", "cron", "sessions_list", "sessions_history",
      "sessions_send", "sessions_spawn", "sessions_yield", "subagents",
      "web_fetch", "session_status", "memory_get", "memory_search",
      "anolisa_memory_search", "anolisa_memory_get", "memory_observe",
      "memory_get_context",
      "other-mcp-server__*"
    ],
    "exec": { "security": "full", "ask": "off" }
  }
}
```

| Policy | Role |
|--------|------|
| `tools.allow` | Explicit allowlist: `exec` (enables tool execution) + task MCP tools in `serverKey__toolName` format (double underscore). Only listed tools are available to the model. |
| `tools.deny` | Blocks all built-in gateway tools (host exec/read/write/browser/etc.), the agent-memory plugin's persistent-memory tools, and other MCP servers' tools. Prevents host filesystem access, cross-task memory recall, and cross-task leakage. |
| `tools.exec` | Sets MCP tool execution security policy (required for tool calls to succeed). |

### Two MCP Servers Per Task

| Server | Purpose | Tool Examples |
|--------|---------|---------------|
| `claw-eval-mock-<task_id>` | Mock services (email, calendar, etc.) running on host | `gmail_list_messages`, `calendar_create_event` |
| `claw-eval-sandbox-<task_id>` | Sandbox bridge routing to Docker container | `Bash`, `Read`, `Write`, `Edit`, `Glob`, `Grep` |

Both are registered via `openclaw mcp set` (persisted, stdio transport). The sandbox bridge routes all file/shell operations to an isolated Docker container — even when the agent calls `Bash`, it executes inside the container, not on the host.

### Batch Cross-Task Isolation

In batch mode, each task's deny list also includes other tasks' MCP server names (`claw-eval-mock-<other_task>__*`), preventing one agent from calling another task's mock services.

### Host Memory Isolation

Where the agent-memory OpenClaw plugin is installed and enabled, it registers its tools directly on the gateway instead of through an MCP server, so the `claw-eval-*__*` wildcards above never match them — the same reason the host `memory-core` plugin's `memory_get` / `memory_search` are denied by plain name. Its store persists across tasks, runs, and gateway restarts, which would let one agent read what an earlier task recorded and leave answers behind for the next one. Every task therefore denies all four plugin tools:

| Denied | Provided by |
|--------|-------------|
| `anolisa_memory_search`, `anolisa_memory_get` | agent-memory ≥ 0.2.8 (namespaced names) |
| `memory_observe`, `memory_get_context` | agent-memory (all versions) |
| `memory_get`, `memory_search` | agent-memory ≤ 0.2.7 and OpenClaw's own `memory-core` |

The last row is why both spellings stay in the list: `memory_get` / `memory_search` were the plugin's own read/search names before 0.2.8 and are also the host `memory-core` built-ins, so denying them covers the old plugin and the host backend at once.

### Implementation

Core logic: `src/ce_runner/tool_injector.py`
- `_build_allowlist()` — constructs `alsoAllow` from task.yaml tools + sandbox tools
- `_build_deny_list()` — constructs the *extra* `deny` entries from other MCP servers
- `_DENY_BUILTIN_TOOLS` — static list of gateway built-in tools to block
- `_DENY_MEMORY_PLUGIN_TOOLS` — static list of agent-memory plugin tools to block
- `_DENY_HOST_TOOLS` — built-ins + memory plugin tools; the deny baseline `build_agent_tools()` starts from

## Scripts

| Script | Description |
|---|---|
| `scripts/setup_env.py` | One-shot environment setup (see table above) |
| `scripts/configure_model.py` | Configure model API key / base-url / model-id |
| `scripts/configure_openclaw.py` | Configure openclaw settings for ce-runner |
| `scripts/run_integration_test.py` | End-to-end integration test with timestamp & score checks |
| `scripts/run_task_compare.py` | Run a task in native + ce-runner modes for comparison |
| `scripts/list_tasks.py` | List tasks grouped by prefix (T/M/C) and difficulty |
| `scripts/debug_task.py` | Single-task interactive debug with verbose output |
| `scripts/analyze.py` | Analyze batch trace artifacts |
| `scripts/summarize_results.py` | Summarize batch results across runs |
| `scripts/generate_trial_reports.py` | Generate per-trial detailed reports |
| `scripts/prompt_task.py` | Display the system prompt for a given task |
| `scripts/check_api_key.py` | Test API key connectivity |
| `scripts/check_openclaw_env.py` | Inspect openclaw environment (`--fix` to cleanup) |

## Troubleshooting

| Issue | Fix |
|---|---|
| `uv` command not found after setup | `python -m pip install --upgrade uv` (rerun setup) |
| `command not found: ce-runner` | use `uv run ce-runner ...` instead of bare `ce-runner` |
| Gateway service disabled / not installed | rerun `python scripts/setup_env.py` (auto installs and starts) |
| Gateway running but unreachable | `openclaw gateway restart` |
| Docker permission denied | `sudo usermod -aG docker $USER && newgrp docker` |
| Sandbox image build slow / fails on TUNA | already handled — setup patches the Dockerfile to official PyPI; restore via `Dockerfile.agent.bak` if needed |
| Environment polluted | `uv run python scripts/check_openclaw_env.py --fix` |
| Mock services port conflict (9100–9116) | `ss -tlnp \| grep 91` to locate, then kill |
