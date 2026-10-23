# semantic-arbiter

[中文版](README_zh.md)

Agent-independent semantic judgments with configurable decision-model backends and deterministic recorded-clock checks. The Rust library evaluates preservation and task-scope questions against a frozen task, candidate and evidence. Bailian and TypeSafe JEV adapters interpret task intent under deterministic resource constraints and local reconstruction requirements. Results are advisory; consumers own authorization and execution.

## Use

- `check` deterministically evaluates frozen structured inputs at `check_at_ms`; it does not replay an Agent or its actions. It uses `semantic-arbiter/v1alpha1` with mode `deterministic_check`.
- `evaluate` retains `semantic-arbiter/v1alpha2` model evaluation under a real-time deadline. Deterministic hard blockers remain `block`; eligible model ambiguity, timeout, or backend/response failure produces an `unknown` answer and candidate `nop`, never `ask`.
- `describe` lists supported protocols, scenarios, backends and message limits as JSON.
- `--help` shows human-readable usage and examples; `help <command>` explains each command.

Scenario selection uses `preservation` and `task_scope`. Current `check` outcomes are `allow/block`; current `evaluate` outcomes are `nop/allow/block`. The full CandidateControl contract for every operation and future scenario is ordered `nop < allow < ask < block`. `ask` is a deterministic approval decision, not an ambiguity fallback; its types, validation and Approval Directive projection are reserved, while runtime `ApprovalExecutor` and Adapter admission remain pending A4/A10. Live evidence collection, dynamic registration, configurable composition, Executors and an AW adapter also remain pending. Model probability thresholds are uncalibrated decision parameters; they do not establish semantic accuracy or safety guarantees.

## Source quick start

Installer and RPM distribution are unavailable. For a source build, use a Linux checkout with Rust/Cargo, rustfmt, clippy and Make. From the repository root:

```bash
cd src/semantic-arbiter
make build
./target/debug/semantic-arbiter describe
make check
```

See the [CLI guide](../../docs/user-guide/en/user-entrypoint/semantic-arbiter.md) for configuration, synthetic inputs, commands and errors, and the [protocol reference](docs/design/protocol.md) for field definitions and validation rules. Backend configuration selects `bailian` or `typesafe`; `thresholds.preservation_ppm` and `thresholds.task_scope_ppm` default to `900000`. The Bailian example explicitly sets both to `800000` and supplies a workspace-scoped endpoint template.

## Module boundaries

| Module | Responsibility |
|---|---|
| `contract` | Task/candidate binding, evidence provenance, budgets and unit answers |
| `config` | Backend selection, endpoint configuration, credential environment-variable names and thresholds |
| `model` | Noul probabilities, backend interface, explicit test fixtures and bounded HTTP transport |
| `judgment` | Versioned preservation and task-scope questions and probability interpretation |
| `evaluation`, `deadline` | Model evaluation, retained observations and original real-time deadlines |
| `outcome` | Typed recommendations, Directive projection and three-state receipts |
| `protocol`, `validation`, `wire` | Capabilities, decoding, binding and semantic validation |
| `check` | Deterministic recorded-clock checks and fixed recommendation templates |
| `main` | Bounded CLI input, backend construction and sanitized JSON output |

One Cargo package contains the library, CLI and tests, with no AW or ws-ckpt path dependency. Python is needed only for the synthetic request generator. Use the public decoding and validation entry points. Model-response verification requires the original request, configuration and independently retained `EvaluationRecord`; exported traces alone provide no authentication.

Consumers must independently authenticate task origins and evidence provenance, retain immutable bindings, and verify authorization, deadlines and effects before executing directives. `Outcome::directives` only projects recommendations. An answer `unknown`, candidate `nop`, candidate `ask`, and an execution receipt `unknown` are distinct states. Exit zero means a complete judgment, not permission. The component outputs neither secrets nor raw upstream response bodies. Remote availability and model accuracy require separate authorized validation.
