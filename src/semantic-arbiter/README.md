# semantic-arbiter

[中文版](README_zh.md)

Agent-independent semantic judgments with configurable decision-model backends and deterministic recorded-clock checks. The Rust library evaluates preservation, task-scope and snapshot questions against a frozen task, candidate and evidence. Bailian and TypeSafe JEV adapters interpret the authoritative task text and the candidate modification under optional resource constraints, with local rebuild evidence supplied as data. Results are advisory; consumers own authorization and execution.

## Use

- `check` deterministically evaluates frozen structured inputs at `check_at_ms`; it does not replay an Agent or its actions. It uses `semantic-arbiter/v1alpha1` with mode `deterministic_check`.
- `evaluate` uses the same `semantic-arbiter/v1alpha1` for model evaluation under a real-time deadline. Deterministic hard blockers remain `block`; eligible model ambiguity, timeout, or backend/response failure produces an `unknown` answer and candidate `nop`, never `ask`.
- `describe` lists supported protocols, scenarios, backends and message limits as JSON.
- `--help` shows human-readable usage and examples; `help <command>` explains each command.

A candidate lists 0–64 `changes`, each `{"operation":"replace"|"delete","resource_id":...}`, naming every resource the call affects once; an empty list is the honest projection of a fully attributed read-only call. Both request formats optionally accept `candidate.native_tool: {"name":"Bash","input":{"command":"printf test"}}`. Supply the complete native argument object, not an extracted command summary. The name is nonblank and at most 128 UTF-8 bytes; compact input JSON is at most 65536 bytes. When present, set `binding.candidate_revision` with `contract::candidate_revision(&candidate)` and retain the same binding in evidence. The domain-separated SHA-256 covers every listed change in order and the recursively key-sorted native data. Task-scope evaluation considers every argument as untrusted data, never instructions or authority, and executes none of it. Without `native_tool`, serialization omits the field and caller-owned revisions remain accepted.

Scenario selection uses any distinct subset of `preservation`, `task_scope` and `snapshot`. In this source implementation, `evaluate` keeps missing tasks, declared resource constraints and stale, untrusted or conflicting rebuild evidence as hard gates. Missing or inconclusive evidence and coverage gaps instead remain visible while eligible units fall back to semantic judgment; task scope needs no rebuild evidence. Optional candidate-bound `LocalContext` supplies bounded observations, not authorization. Built-in local policies avoid model questions when local facts settle a unit: snapshot tries `no_loss_read_only`, `no_current_bytes_lost`, `protect_unique_bytes`, `protect_preexisting_bytes` and `protect_working_state`; preservation tries `preserve_no_mutation` and `preserve_absent_target`. Task scope has no local policy: goal alignment is semantic, so reads always ask the model, with `read_only`/`sensitive_read` as hints. Remaining questions use at most one backend call. Traces expose `route`, `policy_id` and `baseline_reason`; see the [protocol rules](docs/design/protocol.md#judgment-rules-and-model-exchange). These policies add no user configuration settings. `check` is unchanged: deterministic unknowns fail closed and it never recommends a snapshot; only a completed `evaluate` judgment does. Current `check` outcomes are `allow/block`; current `evaluate` outcomes are `nop/allow/block`. The full CandidateControl contract for every operation and future scenario is ordered `nop < allow < ask < block`. `ask` is a deterministic approval decision; ambiguity never produces it. Its types, validation and Approval Directive projection are reserved, while real approval execution and Adapter admission remain pending consumer integration.

The public library now includes four explicit extension contracts. `EvidenceSourceRegistry` freezes cooperatively collected, deadline-bounded source output. `JudgmentUnitRegistry` holds exactly one unit for each closed existing scenario, so consumers may replace the preservation, task-scope or snapshot unit without adding arbitrary wire scenarios or using dynamic discovery. `Evaluator::with_units` uses one registry for question preparation, revision-bound configuration digests and answer derivation; `EvaluationRecord` freezes the expected response for later validation, and central validation rejects invalid custom answers. `composition::compose_outcome` deterministically combines typed contributions, and the consumer-side executor API admits, claims, dispatches and independently queries typed directives. Production evidence sources, real Executors, native Adapter integration, and persistent production claims, audit and effect verification remain pending consumer integration. Model probability thresholds are uncalibrated decision parameters; they do not establish semantic accuracy or safety guarantees.

## Source quick start

Installer and RPM distribution are unavailable. For a source build, use a Linux checkout with Rust/Cargo, rustfmt, clippy and Make. From the repository root:

```bash
cd src/semantic-arbiter
make build
./target/debug/semantic-arbiter describe
make check
```

The repository also provides runnable developer examples for replacing one closed-scenario judgment unit, exercising the in-memory executor fixture and collecting from an in-memory EvidenceSource:

```bash
cargo run --example judgment_unit
cargo run --example executor_fixture
cargo run --example evidence_source
```

See the [CLI guide](../../docs/user-guide/en/semantic-arbiter/index.md) for configuration, synthetic inputs, commands and errors, and the [protocol reference](docs/design/protocol.md) for field definitions and validation rules. Backend configuration selects `bailian` or `typesafe`; `thresholds.preservation_ppm`, `thresholds.task_scope_ppm` and `thresholds.snapshot_ppm` default to `900000`. The Bailian example explicitly sets all three to `800000` and supplies a workspace-scoped endpoint template.

## Module boundaries

| Module | Responsibility |
|---|---|
| `contract` | Task/candidate binding, evidence provenance, budgets and unit answers |
| `evidence` | Explicit source registration, bounded collection, frozen evidence and coverage gaps |
| `config` | Backend selection, endpoint configuration, credential environment-variable names and thresholds |
| `model` | Noul probabilities, backend interface, explicit test fixtures and bounded HTTP transport |
| `judgment` | Closed-scenario unit registry, component-owned questions and centrally validated probability interpretation |
| `local_policy` | Candidate-bound local context, per-unit local policies and decision routes |
| `evaluation`, `deadline` | Hard gates, local decisions, semantic fallback, frozen records and original deadlines |
| `composition` | Deterministic contribution merging and granular capability declarations |
| `executor` | Consumer authorization, idempotent claim/dispatch/query contracts and in-memory fixtures |
| `outcome` | Typed recommendations, Directive projection and three-state receipts |
| `protocol`, `validation`, `wire` | Capabilities, decoding, binding and semantic validation |
| `check` | Deterministic recorded-clock checks and fixed recommendation templates |
| `main` | Bounded CLI input, backend construction and sanitized JSON output |

One Cargo package contains the library, CLI and tests, with no AW or ws-ckpt path dependency. Python is needed only for the synthetic request generator. Use the public decoding and validation entry points. Model-response verification requires the original request, configuration and independently retained `EvaluationRecord`, which binds the original configuration and frozen expected response without invoking a unit or model again; exported traces alone provide no authentication. The CLI keeps the built-in units, fixed composition and existing protocol versions.

Consumers must independently authenticate task origins and evidence provenance, retain immutable bindings, and verify authorization, deadlines and effects before executing directives. `Outcome::directives` only projects recommendations. The executor contract accepts a trusted `AuthorizationContext`, requires a supported protocol version and granular capabilities, and binds each idempotency key to `api_version`, `request_id`, the full `Binding` and the immutable `Directive`. Existing claims are queried without another dispatch; transport success does not establish an applied effect, and unknown effects are never replayed automatically. `executor::fixture` is for in-memory tests and examples and proves no native Adapter support. An answer `unknown`, candidate `nop`, candidate `ask`, and an execution receipt `unknown` are distinct states. Exit zero means a complete judgment, not permission. `EnvironmentBackend` reads credentials only when inference is needed: purely local evaluation needs none, and missing credentials on an eligible model path produce a valid `unknown`/`nop` result with exit zero unless a hard blocker applies. The component outputs neither secrets nor raw upstream response bodies. Fixture probabilities test contracts, not model efficacy or native end-to-end integration.
