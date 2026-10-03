# Benchmark

[中文版](README_zh.md)

This directory contains the evaluation runners for AI-agent benchmarks.
Component-specific benchmarks stay with their components — the Tokenless
compression suites (L1 compressor, L2 module, L3 scenario) live under
`src/tokenless/benchmark/`.

## Runners

| Directory | Benchmark | Notes |
|---|---|---|
| `claweval-runner/` | ClawEval (the OpenClaw platform's evaluation suite) | Task runner and result tooling; pins openclaw 2026.4.22, needs Docker and Python ≥ 3.11 |
| `swe-runner/` | SWEBench | Agent + evaluation harness (cosh/openclaw agents, trace extraction) for real-world software engineering tasks |
| `terminal-runner/` | TerminalBench | Adapter built on a pinned harbor checkout; `scripts/setup.sh` clones harbor and the terminal-bench dataset |

## AI Bench Agents

The runners provide agent implementations for mainstream AI benchmarks.
All agents have been adapted to the OpenClaw platform and specifically
optimized for the task types within each bench to improve instance
execution stability and pass rates.

Currently supported benchmarks include:

- **SWEBench** — A benchmark targeting real-world software engineering tasks. The agent is optimized for code comprehension, fault localization, and patch generation workflows with enhanced orchestration and fault tolerance.
- **TerminalBench** — A benchmark focusing on terminal interaction tasks. The agent is adapted for command execution, output parsing, and multi-step interactive scenarios with stability hardening.
- **ClawEval** — The OpenClaw platform's proprietary evaluation suite. The agent covers its diverse task types with tailored prompt strategies and execution logic tuning.

Additional mainstream benchmarks will be continuously integrated, and
new agent implementations will follow the same adaptation and
optimization paradigm.

Each runner keeps its own README with setup and usage instructions.
