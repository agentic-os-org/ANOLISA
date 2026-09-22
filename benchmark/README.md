# Benchmark

This directory contains all source code related to AI benchmark execution and Tokenless capability testing, organized into the following two major sections.

## AI Bench Agents

This section provides agent implementations for mainstream AI benchmarks. All agents have been adapted to the OpenClaw platform and specifically optimized for the task types within each bench to improve instance execution stability and pass rates.

Currently supported benchmarks include:

- **SWEBench** — A benchmark targeting real-world software engineering tasks. The agent is optimized for code comprehension, fault localization, and patch generation workflows with enhanced orchestration and fault tolerance.
- **TerminalBench** — A benchmark focusing on terminal interaction tasks. The agent is adapted for command execution, output parsing, and multi-step interactive scenarios with stability hardening.
- **ClawEval** — The OpenClaw platform's proprietary evaluation suite. The agent covers its diverse task types with tailored prompt strategies and execution logic tuning.

Additional mainstream benchmarks will be continuously integrated, and new agent implementations will follow the same adaptation and optimization paradigm.

## Tokenless Functional & Performance Testing

This section validates the context compression capabilities provided by Tokenless across different task stages. Tests are organized into two tiers:

### Module-Level Testing

Independent functional and performance verification of each Tokenless compression module, covering compression effectiveness and correctness at various task stages (e.g., context construction, intermediate reasoning, result generation). This ensures each module meets expected performance at the unit level.

### End-to-End Testing

Selected representative business scenarios are used to exercise the complete task pipeline end-to-end, verifying Tokenless's overall compression capability, information fidelity, and impact on final task outcomes under realistic workloads. This ensures the full processing chain operates cohesively as expected.

