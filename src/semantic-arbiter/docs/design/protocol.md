# Semantic judgment protocol

[中文版](protocol_zh.md)

## Scope and ownership

`semantic-arbiter` evaluates frozen task, candidate and evidence inputs and returns advisory judgments. Consumers own input authentication, immutable identity/version bindings, authorization and execution verification.

| Entry point | Protocol / mode | Evaluation |
|---|---|---|
| `check` | `semantic-arbiter/v1alpha1` / `deterministic_check` | Deterministic constraints and evidence checks at the recorded `check_at_ms` |
| `evaluate` | `semantic-arbiter/v1alpha2` / `model_evaluation` | The same deterministic hard gate followed by eligible model questions under a real-time deadline |
| `describe` | Capability declaration | Supported operations, versions, modes, scenarios, backends, candidate controls, obligation kinds and message size limit |

Both operations use inline frozen evidence. `check` performs no external I/O and does not replay an Agent or its actions; model evaluation sends selected input to the configured backend. Neither operation collects filesystem facts or executes directives. Callers select `preservation`, `task_scope`, or both. Current `check` producers emit candidate `allow/block`; current `evaluate` producers emit `nop/allow/block`. The full CandidateControl interface remains `nop < allow < ask < block` for every operation and future scenario.

The library implements explicit EvidenceSource collection, closed-scenario JudgmentUnit replacement, deterministic Outcome composition and consumer-side executor contracts. The CLI keeps the built-in units, fixed composition, behavior and protocol versions shown above. Production evidence sources, real Executors, native Adapter integration, persistent production claims and audit, and effect verification remain pending consumer integration.

Public definitions are in [`contract.rs`](../../src/contract.rs), [`evidence.rs`](../../src/evidence.rs), [`config.rs`](../../src/config.rs), [`judgment.rs`](../../src/judgment.rs), [`evaluation.rs`](../../src/evaluation.rs), [`composition.rs`](../../src/composition.rs), [`executor.rs`](../../src/executor.rs), [`outcome.rs`](../../src/outcome.rs) and [`protocol.rs`](../../src/protocol.rs). Use their public decoding and validation entry points to enforce semantic constraints.

## Request fields

### Envelope, binding and task

| Field | Meaning and constraint |
|---|---|
| `api_version`, `mode` | Exact protocol/mode pair from the table above |
| `request_id` | Consumer-owned immutable request identity, echoed in responses and receipts |
| `binding.session_id` | Session owning the task and candidate |
| `binding.instance_id` | Stable process/session instance identity; consumers must resolve and authenticate it independently |
| `binding.workspace_id` | Consumer-verified opaque workspace identity |
| `binding.task_version` | Current task version, retained independently of history |
| `binding.config_revision` | Consumer policy revision; model configuration must carry the same value |
| `binding.candidate_id`, `binding.candidate_revision` | Frozen candidate identity and revision; changing candidate inputs requires a new revision |
| `scenarios` | Nonempty, distinct subset of `preservation`, `task_scope`; Rust variants are `Scenario::Preservation` and `Scenario::TaskScope` |
| `task` | Authoritative task projection; `null` produces an unknown answer with reason `missing_task` |
| `task.session_id`, `task.version` | Must equal `binding.session_id` and `binding.task_version` |
| `task.origin` | `operator` or `native_user`; the consumer must authenticate the ingress |
| `task.text` | Nonblank task intent, at most 16384 UTF-8 bytes; interpreted only by model evaluation |
| `task.allowed_resource_ids` | Explicit resource ceiling for the candidate operation |
| `task.preserve_resource_ids` | Resources required to remain unchanged; retention overrides allowance |
| `candidate.operation`, `candidate.resource_id` | `replace` or `delete` on an opaque resource identity under the bound workspace |

Bindings must remain immutable. Every evidence binding must exactly equal the request binding. Task text, source reports and model content cannot expand the structured resource ceiling or grant execution authority.

### Evidence, gaps and budget

| Field | Meaning and constraint |
|---|---|
| `evidence[].id` | Unique evidence identity referenced by answers and outcomes |
| `evidence[].source_id`, `source_revision` | Source identity and immutable content revision supplied by the consumer |
| `evidence[].provenance` | `local_verifier`, `source_reported` or `model_echo`; only `local_verifier` qualifies for reconstruction checks |
| `evidence[].binding` | Exact association with the request's task, candidate, instance, workspace and policy |
| `evidence[].captured_at_ms`, `valid_until_ms` | Collection time and exclusive expiry; collection must precede expiry |
| `evidence[].fact.kind` | `rebuild` |
| `evidence[].fact.parameters.resource_id` | Resource compared by the verifier |
| `evidence[].fact.parameters.result` | `identical`: retained recipe reproduced the bytes; `unique`: existing bytes include unreproduced results; `unknown`: comparison was inconclusive |
| `gaps[].source_id`, `scenario` | Explicit missing coverage for a source and scenario; duplicate pairs are invalid, and gaps for enabled units prevent allowance |
| `budget.opened_at_ms`, `deadline_ms` | Original event opening and exclusive deadline; deadline must follow opening, and evaluation time cannot precede opening |
| `budget.max_evidence_items`, `max_evidence_bytes` | Caller limits on item count and compact serialized evidence-array bytes, bounded by protocol maxima |
| `check_at_ms` | Check-only evaluation time in the fixture's recorded clock; absent from model requests |

A provenance label is an assertion. Consumers must independently authenticate the task ingress, evidence source and verifier result. Synthetic fixtures only assert these facts. `source_reported` and `model_echo` cannot establish local reconstruction.

`check` uses the recorded clock. Model requests use Unix milliseconds for budget and evidence timestamps. Evidence is usable only when `captured_at_ms <= evaluation_time < valid_until_ms`. In `check`, expired event budgets, exceeded caller evidence limits, or other deterministic unknowns fail closed as candidate `block`. Malformed timestamps, hard size violations and binding mismatches are protocol errors.

### Pre-evaluation evidence collection

`EvidenceSourceRegistry` is an optional producer-side library API, not part of either wire protocol. Consumers explicitly register in-process sources and select source IDs before finalizing a check or evaluation request. Collection receives the frozen request context without evidence or gaps. The registry injects the registered source identity, revision, provenance and request binding; source output cannot replace them. It returns deterministically ordered inline `evidence`, `gaps`, sanitized per-source reports and one final `collected_at_ms`.

Every cooperatively asynchronous source gets one attempt under the minimum of its registered timeout and the original event deadline. Source implementations must return their future promptly and must not block its executor thread; Tokio deadlines cannot preempt blocking in-process code. There is no retry, fallback, implicit source, plugin discovery or deadline renewal. Unavailable, timed-out, future, expired or over-age source batches become explicit gaps for the selected supported scenarios. The final freeze instant rechecks every batch, so time spent collecting a later source cannot leave earlier expired evidence in the result. Invalid source output, duplicate evidence IDs and item or compact-array byte budget excess fail the whole collection without truncation.

Collection does not authenticate a source or promote provenance. The in-memory example remains `source_reported`, so its rebuild claim cannot satisfy the local-verifier requirement. Consumers place the returned evidence and gaps into the request; a deterministic request also sets `check_at_ms` to the returned `collected_at_ms`. `check` and `evaluate` never invoke the registry themselves.

### Wire bounds

| Limit | Value |
|---|---|
| Contract message | 262144 UTF-8 bytes |
| Evidence items, gap records, IDs per list, obligations | At most 64 each |
| Subject actions | At most one each of `freeze`, `quarantine`, `kill` |
| Identifier | 1–128 ASCII bytes: letters, digits, `.`, `_`, `-`, `:`, `@` |
| Unit confidence | Integer `0..=1000000` |

Contract objects are closed; unknown fields, duplicate fields, unknown enum values and trailing JSON are rejected. Unsigned integer fields reject negative or fractional values. Tagged facts and actions use `kind` and `parameters`. Model request, configuration and upstream-reply decoding additionally rejects duplicate keys at every depth, including ignored reply metadata. Input JSON formatting is independent of the typed serialization used for digests.

## Backend configuration

Configuration is a separate trusted input. Evidence cannot select a backend, endpoint, credential variable or threshold.

| Field | Meaning and default |
|---|---|
| `config_revision` | Policy label matching `binding.config_revision`; callers must revise it when policy changes |
| `backend.kind` | `bailian` or `typesafe`; fixtures require explicit in-process injection |
| `backend.endpoint_url` | Bailian's required complete inference URL, used unchanged |
| `backend.base_url` | TypeSafe base URL, default `https://api.typesafe.ai`; trailing slashes are trimmed before appending `/v1/systemone` |
| `backend.model` | Configured name/alias; defaults to `decision-model-preview` for Bailian or `jev-latest` for TypeSafe |
| `backend.key_env` | Credential environment-variable name; defaults to `DASHSCOPE_API_KEY` or `TYPESAFE_API_KEY` |
| `timeout_ms` | Required integer `1..=120000`; caps preparation and evaluation from ingress and can only shorten the original deadline |
| `max_response_bytes` | Required integer `1..=262144`; enforced while reading the upstream body |
| `thresholds.preservation_ppm`, `thresholds.task_scope_ppm` | Independent integer support thresholds in `500001..=1000000`, each defaulting to `900000` |
| `transport` | Default `https`; `loopback_test` permits HTTP only on literal loopback IP addresses |

The Bailian example uses `https://{WorkspaceId}.cn-beijing.maas.aliyuncs.com/compatible-mode/v1/systemone` and explicitly sets both thresholds to `800000`. Supply the authorized workspace ID and verify the complete endpoint before use. These thresholds are uncalibrated decision parameters; model accuracy requires separate measurement.

URLs reject embedded credentials, query strings and fragments. HTTPS uses certificate verification. The HTTP adapter is configured to disable redirects, environment proxies, retries and idle connection pooling. It loads only the configured environment variable and never loads `.env`. Secrets and raw upstream response bodies are excluded from outputs.

## Judgment rules and model exchange

Both units first require a task, apply retention and allowed-resource constraints, and check declared coverage gaps. Preservation also requires matching local-verifier rebuild evidence. Stale, missing, untrusted, conflicting or inconclusive proof yields unknown. Untrusted supplemental evidence cannot replace qualified proof. Task scope needs no rebuild evidence.

| Unit / question ID | Model proposition | Supported result |
|---|---|---|
| `preservation` / `preservation-v1` | Does task intent require keeping the resource unchanged against this operation? | Yes support meeting the threshold yields `preserve`; no support meeting it retains the deterministic `allow` or `needs_snapshot` result |
| `task_scope` / `task-scope-v1` | Does this operation on this resource satisfy task intent? | Yes support meeting the threshold yields `allow`; no support meeting it yields `outside_scope` |

Units run in preservation/task-scope order regardless of request order. `check` derives answers entirely from structured constraints and evidence: `identical` permits modification, while `unique` requires a snapshot; deterministic unknowns fail closed. Model evaluation reuses that deterministic hard gate and batches eligible questions into at most one backend call. Hard blockers remain `block`. For eligible questions, low bilateral confidence, timeout, or backend/model-response error yields an `unknown` unit answer and candidate `nop`; ambiguity is never `ask`. Preservation model abstention retains only `audit` and does not carry the deterministic baseline's `snapshot` into the Outcome.

Both HTTP adapters send Bearer-authenticated POST requests with `{model,state,questions}`. `state` contains `authoritative_task`, `candidate` and selected evidence with provenance. Questions are component-owned `{type:"noul",instructions:...}` objects keyed by versioned question IDs. Endpoint configuration and credentials are excluded from state.

Upstream `answers` must contain exactly the requested question IDs. Each answer has a numeric `noul` and, for TypeSafe, required `type="noul"`. Bailian permits the type to be omitted; a supplied value must be `"noul"`. Optional top-level `model`, `usage`, `request_id` and `latency_ms` are accepted as ignored metadata. Unknown fields, duplicate keys, extra/missing answers and malformed probabilities become sanitized model-response errors: eligible units answer `unknown` and candidate control is `nop` unless the deterministic hard gate already requires `block`. Returned model metadata cannot establish backend identity.

Only Noul answers are supported. Each value represents an independent P(yes) for one proposition; probabilities across questions need not sum to one. Values must be finite JSON numbers in `[0,1]`. Strings, booleans, nulls and out-of-range numbers are rejected. `noul-floor-ppm/v2` preserves the exact decimal token, including exponent notation, and computes `yes_ppm=floor(p*1000000)` and `no_ppm=floor((1-p)*1000000)` before floating-point conversion. Threshold comparisons use these exact floors. `raw_json_number()` provides lossless access; `raw()` is a lossy convenience accessor. Probability and unit confidence express model support, with no established calibration to factual correctness or safety.

### Judgment-unit registry

`JudgmentUnitRegistry` is an explicit process-local replacement point for the two closed `Scenario` values. Its constructor requires exactly one `preservation` unit and exactly one `task_scope` unit, with nonempty revisions and nonempty distinct question IDs. The registry cannot add an arbitrary wire scenario and performs no plugin or dynamic code discovery. A consumer replacing one unit supplies the built-in or another reviewed implementation for the other slot.

`Evaluator::with_units` uses the supplied registry while preparing questions and deriving the response. Each unit's declared revision, question ID and question content enter `config_digest`. Once evaluation stabilizes, `EvaluationRecord` freezes the expected response and the original configuration digest; later response validation compares those immutable values and does not invoke a unit or model again. Central answer validation requires the original unit and evidence references, bounds confidence to `0..=1000000`, and permits only the value/reason pairs defined for that closed scenario. A custom unit returning any other answer causes `invalid_response`.

## Response fields and verification

### Common response and unit answers

| Field | Meaning |
|---|---|
| `api_version`, `mode`, `request_id`, `binding` | Original protocol, evaluation mode and immutable input association |
| `input_digest` | Domain-separated SHA-256 binding all typed request content |
| `answers[].unit` | Enabled scenario; exactly one answer per enabled unit |
| `answers[].value` | Unit conclusion: `allow`, `preserve`, `needs_snapshot`, `outside_scope` or `unknown` |
| `answers[].confidence_ppm` | Selected proposition support for model-derived answers; deterministic known answers use `1000000`, unknown uses `0` |
| `answers[].reason` | Stable explanation code, such as `allowed`, `preserved`, `needs_snapshot`, `missing_evidence` or `budget_exceeded` |
| `answers[].evidence_refs` | Locally selected IDs from this request's frozen evidence |
| `outcome` | Candidate/subject control, ordered obligations and evidence references |

A unit answer describes that unit's conclusion; it is not itself CandidateControl. Candidate disposition is the aggregate `outcome.control.candidate`. For `check`, any `preserve`, `outside_scope` or deterministic `unknown` answer produces `block/[audit]`; with no blocker, `needs_snapshot` produces `allow/[snapshot,audit]`, otherwise `allow/[audit]`. For `evaluate`, deterministic hard blockers remain `block/[audit]`; when an eligible model question abstains through low bilateral confidence or failure, its answer is `unknown` and the current producer returns candidate `nop/[audit]`. In particular, preservation abstention does not preserve a baseline snapshot recommendation. Completed, nonblocking model judgments can produce `allow`, including `allow/[snapshot,audit]` where the preservation judgment supports it. A block suppresses the snapshot recommendation. Deterministic confidence of `1000000` records a completed rule result under asserted inputs and provides no provenance authentication.

An `allow` remains advisory. When it includes `snapshot`, the consumer must hold the candidate, independently authorize and confirm the snapshot, and revalidate binding, evidence and deadline before execution. Successful inference, approval or response delivery cannot replace execution authorization.

### Model-evaluation response additions

| Field | Meaning |
|---|---|
| `config_digest` | Digest of nonsecret configuration plus question and interpretation versions; distinct from the caller's `config_revision` label |
| `backend.kind`, `backend.mode`, `backend.model` | Constructed adapter kind (`bailian`, `typesafe`, `fixture`), mode (`remote`, `loopback_test`, `fixture`) and configured model name/alias |
| `normalization_revision` | Probability conversion version, `noul-floor-ppm/v2` |
| `evaluated_at_ms` | Final validity-check time in Unix milliseconds |
| `elapsed_ms` | Monotonic elapsed time through evaluation, including input preparation |
| `model_called` | Whether the selected backend was invoked; false when no eligible question is called |
| `judgments[].unit`, `unit_revision`, `question_id` | Unit identity, the registered unit's declared interpretation revision and versioned question identity |
| `judgments[].threshold_ppm` | Configured support threshold for that unit |
| `judgments[].probability` | Validated `{raw,yes_ppm,no_ppm}`, or null when unavailable |
| `judgments[].error` | Sanitized backend error or null; examples include `timeout`, `authentication`, `rate_limited`, `transport`, `http`, `invalid_response`, `response_too_large` |
| `judgments[].model_used` | A validated probability was used in deriving the final unit answer; false after expiry or a deterministic rejection |
| `judgments[].evidence_refs` | Locally selected evidence references for the unit |

The CLI captures wall and monotonic clocks at ingress. Preparation, inference, body reads, validation and composition consume one deadline bounded by the original event and configured timeout. Evidence is rechecked after inference and composition. Exhaustion after an eligible model path yields an `unknown` answer and candidate `nop` unless a deterministic hard blocker already requires `block`. A probability can remain in the trace with `model_used=false` when its evidence or budget expires.

Retain `EvaluationRun::record()` independently. `evaluation::decode_response` and `validate_response` require the original request, configuration and non-deserializable local `EvaluationRecord`, then compare against the frozen expected response without invoking a unit or model again. Backend injection is a trusted in-process API; callers must bind the actual adapter to its configuration. Wire traces and backend labels alone provide no authentication across an untrusted process boundary. Record validation establishes recorded consistency; consumers must check freshness again before execution. Check response validation recomputes the deterministic result from the retained request.

### Digests

Digests are `sha256:` followed by lowercase SHA-256 over a domain separator and compact, declaration-order typed JSON. Arrays, enum values and nulls are preserved. Cross-language implementations must match this serialization and the repository's digest vectors; hashing incoming whitespace or using RFC 8785 produces a different contract.

| Digest | Domain separator | Typed payload |
|---|---|---|
| Check `input_digest` | `semantic-arbiter/v1alpha1\0check-input\0` | `CheckRequest` |
| Evaluation `input_digest` | `semantic-arbiter/v1alpha2\0evaluation-input\0` | `EvaluationRequest` |
| Evaluation `config_digest` | `semantic-arbiter/v1alpha2\0evaluation-config\0` | Array containing configuration, framework unit revision, normalization revision, and ordered scenario/question ID/unit revision/question definitions |
| `ModelConfig::digest()` | `semantic-arbiter/v1alpha2\0model-config\0` | Configuration alone; separate from the response's `config_digest` |

`\0` denotes a zero byte. `config_revision` is a caller-maintained policy label; `config_digest` binds effective configuration and versioned semantics. Credential values are absent from these payloads. Digests bind content and provide no sender authentication.

## Outcome, Directive and receipt fields

### Deterministic composition

`composition::compose_outcome` combines independently formed `OutcomeContribution` values without authorization or execution. Candidate control is the maximum under `nop < allow < ask < block`. Subject controls form a bounded union with at most one exact request for each of `freeze`, `quarantine` and `kill`. Obligations are exact-deduplicated by ID, and evidence references form a sorted set.

Composition rejects unavailable granular capabilities, workspace or instance binding mismatches, empty controlled identifiers, conflicting content under one directive ID or subject action, and collisions with the reserved `candidate-approval` ID. The result contains exactly one audit obligation; composition adds the canonical critical `audit` obligation when none was supplied and the audit capability is declared, while multiple distinct audit requests fail. Subjects are sorted as freeze, quarantine and kill. Obligations are sorted by action class and then ID, with audit last; evidence references are lexicographically sorted. These rules make output independent of contribution order. The current CLI continues to use its fixed built-in composition path.

| Field | Meaning |
|---|---|
| `outcome.control.candidate` | Restriction order `nop < allow < ask < block`: no judgment, no objection, hold for approval, native denial |
| `outcome.control.subject[]` | Independent subject requests, each with `id`, bound `instance_id`, `action` (`freeze`, `quarantine`, `kill`) and nonempty `scope_ids` |
| `outcome.obligations[]` | Ordered recommendations, each with unique `id`, `critical` and typed `action` |
| `outcome.evidence_refs` | Evidence IDs used by the recommendation |
| `Directive.kind`, `parameters` | `approval`, `subject` or `obligation` with its typed body |
| Approval `parameters.id`, `candidate_id` | Reserved ID `candidate-approval` and the bound candidate awaiting approval |
| Subject / obligation `parameters` | Original subject request or obligation, retaining its immutable `id` and action parameters |

The full CandidateControl contract applies to every operation and future scenario, ordered `nop < allow < ask < block`. Current `check` producers emit `allow/block`; current `evaluate` producers emit `nop/allow/block`. Both emit empty subject controls and only `snapshot/audit` obligations. `ask` is a deterministic decision to require approval; ambiguity and `unknown` answers never produce it. Its types, validation and Approval Directive projection are reserved now. Current preservation/task-scope producers do not emit it, and real approval execution and Adapter admission remain pending consumer integration. The wider vocabulary covers future enhancement/approval, security, subject-control and cleanup scenarios. Public deterministic composition is available; execution scheduling, real action implementations and native adoption remain consumer responsibilities. `Outcome::directives` projects ask, subject requests and obligations. The consumer handles candidate blocking through its native interface. Subject requests remain independent; a pending kill does not absorb freeze or quarantine.

| Obligation `action.kind` | `action.parameters` |
|---|---|
| `audit` | Empty object; sanitized judgment and execution metadata |
| `snapshot` | Bound `workspace_id` |
| `rollback` | Bound `workspace_id`, authorized `checkpoint_id` |
| `cleanup` | Bound `workspace_id`, nonempty `checkpoint_ids` |
| `notify`, `diagnose` | Registered `recipient_id` or `diagnostic_id` |
| `retrieve_skill`, `retrieve_memory` | Registered `query_id` |
| `inject_context` | Nonempty reviewed `material_ids` |

Directive IDs are unique within an outcome; `candidate-approval` is reserved. Each nonempty outcome requires exactly one audit, including subject-only control with candidate `nop`. Evidence references or gaps for enabled scenarios also require audit. Audit, snapshot, rollback and cleanup always use fail-closed policy; `critical=true` applies that policy to enhancements as well. IDs require independent resolution, scope checks and authorization. Rollback requires consumer-verified quiescence, fresh state checks and explicit confirmation.

| Receipt field | Meaning |
|---|---|
| `api_version` | `semantic-arbiter/v1alpha1` for the existing check receipt decoder |
| `request_id`, `binding` | Exact retained request association |
| `directive_id` | ID of the immutable directive retained under `(request_id,binding,directive_id)` |
| `state`, `reason` | Only `applied/confirmed`, `rejected/denied`, `unknown/unconfirmed` are accepted |

Consumers create receipts after independent admission and effect verification. `decode_receipt` takes a `CheckRequest` and retained Outcome and checks correlation and state/reason consistency. This API supplies no model-evaluation receipt decoder or operating-system effect verification. Consumers must separately verify capability, identity, scope, authorization, idempotency and deadline. An answer `unknown`, candidate `nop`, candidate `ask`, and receipt `unknown` are four distinct states: incomplete semantic support, no candidate judgment, a deterministic approval requirement, and an unconfirmed execution effect. For a receipt `unknown`, query the original receipt or inspect the effect manually; never automatically repeat the directive. Rejection also grants no retry permission. Neither operation manufactures execution receipts.

### Consumer-side executor lifecycle

The `executor` module is a consumer-side library contract. `AuthorizationContext` is trusted input created by the consumer; evidence, model output and a Directive cannot create or expand it. Admission requires a supported check or evaluation protocol version, the exact granular `DirectiveCapability`, an unexpired deadline, matching candidate/workspace/instance bindings and an allowlist containing every nonempty controlled scope or parameter ID. `ExecutorRegistry` registers exactly one Executor per capability.

`idempotency_key` is a stable domain-separated digest of `api_version`, `request_id`, the complete `Binding` and the complete immutable `Directive`. Execution follows claim, conditional dispatch and query. A new claim dispatches at most once and then queries the effect. An existing claim skips dispatch and queries the original request. A dispatch result never determines the receipt; only the independent query maps to `applied/confirmed` or `rejected/denied`. A failed or unconfirmed query maps to `unknown/unconfirmed`, and an unknown effect is never automatically replayed.

`executor::fixture` supplies an in-memory claim store and scripted Executor solely for tests and examples. It performs no external action and establishes no native Adapter support. Real Executors, native Adapter adoption, durable production claims, persistent audit and operating-system effect verification remain pending consumer integration.

## CLI completion and integration boundary

A complete valid judgment exits `0`, including an `unknown` answer with candidate `nop` caused by model ambiguity or inference failure, and any valid `block`. Zero means completion, not permission. Malformed or missing request/configuration, unusable or missing credentials, invalid bindings and I/O failures exit `2` with sanitized error JSON on stderr. Allowance output uses nonblocking writes bounded by the event deadline, configured cap and adopted evidence expiry. Expiry during output exits `2` with `deadline_exceeded`; discard all partial stdout. Accept only complete responses with exit zero, inspect the candidate disposition, and revalidate before use.

An AW adapter remains pending and belongs in the repository's `providers/` directory. This CLI cannot be configured as an AW Provider. Integration must authenticate task/evidence delivery, retain immutable correlation, map blocking to native denial, and hold candidates until required protection is confirmed and revalidated. Unsupported protection must remain blocking.
