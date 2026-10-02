# terminal-runner

[中文版](README_zh.md)

This guide explains how to test terminal-bench tasks via the [harbor](https://github.com/harbor-framework/harbor) platform, focusing on two implementation approaches — **Installed mode** and **External mode** — their differences, trade-offs, and usage.

---

## Two modes, two support paths

[harbor](https://github.com/harbor-framework/harbor) runs agent evaluations inside Docker containers. It supports two ways to supply the agent:

### Installed mode — harbor native

Built into harbor. The agent implements an `install()` method — harbor calls it during `setup()` to automatically install the agent inside the container at runtime (e.g., Node.js + openclaw via nvm). The Docker image stays clean; installation happens on every trial.

```bash
harbor run --agent openclaw ...
```

- **Who supports it:** Harbor out of the box via `--agent <name>`.
- **Cost:** Agent is re-installed every trial (adds setup time).

### External mode — harbor's extensibility API

Harbor provides `BaseAgent` + `--agent`, an interface that lets you plug in any agent running outside the container — a host CLI, a remote agent behind an HTTP service, anything. The external agent communicates with the container through `environment.exec()`.

```bash
harbor run --agent-import-path external_agent.openclaw_external_agent:OpenClawExternalAgent ...
```

- **Who supports it:** Harbor's `BaseAgent` API. Anyone can implement an external agent.
- **Advantage:** Install the agent once on host, skip per-trial install overhead. No runtime inside the container.

### This repo

A **working demo** of external mode, using OpenClaw as the host-side agent:

```
OpenClaw (host LLM) <-> OpenClawExternalAgent (router) <-> Harbor Docker container
```

The same pattern works for Claude Code, Codex, or any agent CLI.

---

## Install OpenClaw on host

External mode runs the agent CLI on the host. For this repo's `openclaw_external_agent`, that means the `openclaw` CLI on your host PATH. If you write your own `xxx_external_agent.py`, install the corresponding agent CLI instead.

```bash
# Requires Node.js >= 22.19 (Node 24 recommended)
npm install -g openclaw
openclaw --version
```

See [OpenClaw docs](https://github.com/openclaw/openclaw) for API key and model setup.

---

## What's inside

```
terminal-runner/
├── external_agent/
│   ├── __init__.py                  # Package marker (no imports — lazy loading)
│   └── openclaw_external_agent.py   # External agent adapter (extends harbor BaseAgent)
├── scripts/
│   └── setup.sh                     # Clone harbor upstream + dataset, install harbor
├── .gitignore
├── LICENSE
├── README.md
└── README_zh.md
```

### What's NOT here (cloned by `setup.sh`)

- `harbor/` — upstream harbor framework
- `dataset/` — terminal-bench task dataset

---

## Quick start

### Prerequisites

- Python ≥ 3.12
- Docker ≥ 24 with the **compose v2 plugin** (`docker compose version` must work)
- Node.js ≥ 22.19 + `openclaw` CLI on host PATH (external mode only)
- `git`, `git-lfs`

### Install

```bash
git clone https://github.com/alibaba/anolisa.git
cd anolisa/benchmark/terminal-runner
./scripts/setup.sh
```

`setup.sh` does:
1. Checks Python version — if < 3.12, auto-detects conda (Miniconda/Anaconda) and creates a 3.12 environment (env name configurable via `CONDA_ENV_NAME`, default `terminal-runner-py312`)
2. Creates a Python virtual environment at `.venv/`
3. `git clone` harbor upstream → `harbor/`, **checks out the pinned release tag** (`v0.15.0`), then `pip install -e` into the venv
4. `git clone` dataset (via Git LFS) from HuggingFace → `dataset/`

> **Note:** Harbor is pinned to a specific release (`v0.15.0`) for stability. Override with `HARBOR_REF` if needed.

Override sources:

```bash
HARBOR_URL=git@github.com:your-fork/harbor.git \
HARBOR_REF=v0.15.0 \
DATASET_URL=https://huggingface.co/datasets/<org>/<dataset> \
  ./scripts/setup.sh
```

---

## Usage

### External mode (this repo)

```bash
source .venv/bin/activate
export PYTHONPATH=$(pwd)

harbor run \
  --agent-import-path external_agent.openclaw_external_agent:OpenClawExternalAgent \
  -p dataset/nginx-request-logging \
  -m openai/qwen3.6-plus \
  --agent-env OPENAI_BASE_URL=https://dashscope.aliyuncs.com/compatible-mode/v1 \
  --agent-env OPENAI_API_KEY=sk-xxxxx
```

### Installed mode (harbor built-in)

In installed mode, harbor's built-in `openclaw` agent runs OpenClaw **inside** the
container (auto-installed via nvm + npm).  The built-in agent only supports a
fixed set of providers (`openai`, `anthropic`, `nvidia`); use the `openai`
provider with `OPENAI_BASE_URL` / `OPENAI_API_KEY` for any OpenAI-compatible
endpoint (e.g. DashScope).  Qwen models do not support thinking mode, so pass
`--agent-kwarg thinking=off`.

```bash
source .venv/bin/activate

harbor run \
  --agent openclaw \
  -p dataset/nginx-request-logging \
  -m openai/qwen3.6-plus \
  --agent-env OPENAI_BASE_URL=https://dashscope.aliyuncs.com/compatible-mode/v1 \
  --agent-env OPENAI_API_KEY=sk-xxxxx \
  --agent-kwarg thinking=off
```

### How it works (per iteration)

1. Spawns `openclaw --profile <trial-profile> agent --local --json --message ...` on host
2. Streams stdout/stderr with heartbeat + three safety guards (total-timeout, no-output timeout, stdout cap)
3. Extracts bash commands from OpenClaw's response (toolCall or fenced `bash` code blocks)
4. Executes each command in the harbor container via `environment.exec()`
5. Feeds the results back to OpenClaw for the next iteration
6. Stops when OpenClaw outputs `TASK_COMPLETE` or `OPENCLAW_MAX_ITERATIONS` is hit

### Per-trial isolation

Each trial gets its own `~/.openclaw-<profile>` directory (profile name derived from harbor session id), so concurrent trials don't collide.

### Auto-skill

Skill hints are loaded per-task (highest priority first):

1. **`dataset/<task>/skill.md`** — a manual override file. If it exists, its content is injected verbatim (truncated to 3000 chars). Always active.
2. **`dataset/<task>/solution/solve.sh`** — auto-parsed into a step-by-step hint. **Disabled by default**; enable with `SKILL_FROM_SOLUTION=1`.

The dataset directory is resolved via the `DATASET_DIR` env var (default `dataset`, relative to the `harbor run` working directory).

---

## Environment variables

| Variable | Default | Description |
|---|---|---|
| `OPENCLAW_VERSION` | `2026.4.14` | Version string reported by `version()`. |
| `OPENCLAW_AGENT_ID` | `main` | OpenClaw agent-id passed via `--agent`. |
| `OPENCLAW_TIMEOUT` | `600` | Per-call timeout (s) passed as `--timeout` to OpenClaw. Also the floor for the subprocess total-timeout guard. |
| `OPENCLAW_NO_OUTPUT_TIMEOUT` | `500` | Kill the OpenClaw subprocess after this many seconds with zero stdout (API-hang guard). |
| `OPENCLAW_THINKING` | `off` | OpenClaw thinking mode (`off` / `low` / `high`). |
| `OPENCLAW_MAX_ITERATIONS` | `0` | Max agent-loop iterations (0 = unlimited). |
| `OPENCLAW_MAX_STDOUT_BYTES` | `102400` | Kill OpenClaw subprocess if stdout exceeds this (infinite-generation guard). |
| `DOCKER_EXEC_TIMEOUT` | `600` | Per-command `environment.exec()` timeout in seconds. |
| `DATASET_DIR` | `dataset` | Path to the task dataset directory (used to locate `skill.md` / `solution/solve.sh`). |
| `SKILL_FROM_SOLUTION` | `0` | Set to `1` to enable auto-generated skill hints from `solution/solve.sh`. |

---

## License

Apache License 2.0, matching harbor's license.

---

## Acknowledgements

- [harbor](https://github.com/harbor-framework/harbor) — agent evaluation framework
- [terminal-bench](https://github.com/laude-institute/terminal-bench) — task dataset format
- [OpenClaw](https://github.com/openclaw/openclaw) — host-side agent CLI
