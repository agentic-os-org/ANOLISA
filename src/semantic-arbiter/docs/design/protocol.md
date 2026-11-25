# Semantic judgment protocol

[中文版](protocol_zh.md)

## Scope and ownership

`semantic-arbiter` evaluates frozen task, candidate and evidence inputs and returns advisory judgments. Consumers own input authentication, immutable identity/version bindings, authorization and execution verification.

| Entry point | Protocol / mode | Evaluation |
|---|---|---|
| `check` | `semantic-arbiter/v1alpha1` / `deterministic_check` | Deterministic constraints and evidence checks at the recorded `check_at_ms` |
| `evaluate` | `semantic-arbiter/v1alpha1` / `model_evaluation` | Hard integrity/resource gates, local snapshot policies and semantic fallback under a real-time deadline |
| `describe` | Capability declaration | Supported operations, protocol version, modes, scenarios, backends, candidate controls, obligation kinds and message size limit |

Both operations use inline frozen evidence. `check` performs no external I/O and does not replay an Agent or its actions; model evaluation sends selected input to the configured backend. Neither operation collects filesystem facts or executes directives. Callers select any nonempty distinct subset of `preservation`, `task_scope` and `snapshot`. Current `check` producers emit candidate `allow/block`; current `evaluate` producers emit `nop/allow/block`. The full CandidateControl interface remains `nop < allow < ask < block` for every operation and future scenario.

The library implements explicit EvidenceSource collection, closed-scenario JudgmentUnit replacement, deterministic Outcome composition and consumer-side executor contracts. The CLI keeps the built-in units, fixed composition, behavior and protocol version shown above. Production evidence sources, real Executors, native Adapter integration, persistent production claims and audit, and effect verification remain pending consumer integration.

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
| `scenarios` | Nonempty, distinct subset of `preservation`, `task_scope`, `snapshot`; Rust variants are `Scenario::Preservation`, `Scenario::TaskScope` and `Scenario::Snapshot` |
| `task` | Authoritative task projection; `null` produces an unknown answer with reason `missing_task` |
| `task.session_id`, `task.version` | Must equal `binding.session_id` and `binding.task_version` |
| `task.origin` | `operator` or `native_user`; the consumer must authenticate the ingress |
| `task.text` | Nonblank task intent, at most 16384 UTF-8 bytes; interpreted only by model evaluation |
| `task.allowed_resource_ids` | Optional resource ceiling; when non-empty, every omitted resource is out of scope. An empty list declares no ceiling |
| `task.preserve_resource_ids` | Optional retention limit checked before any unit; an empty list declares no retention requirement |
| `candidate.changes` | 0–64 changes, each `{operation, resource_id}`: `replace` or `delete` on an opaque resource identity under the bound workspace. Every resource the call affects appears once, in consumer order; a repeated `resource_id` is `invalid_input`. An empty list is the honest projection of a fully attributed read-only call: it names no resource, so no retention or allowance constraint applies |
| `candidate.native_tool` | Optional full native call, with exactly `name` and `input`; absent and `null` are equivalent |
| `candidate.native_tool.name` | Nonblank native tool name, at most 128 UTF-8 bytes |
| `candidate.native_tool.input` | Complete JSON argument object, at most 65536 compact serialized UTF-8 bytes, with no truncation or command-only projection |

Bindings must remain immutable. Every evidence binding must exactly equal the request binding. Task text, source reports and model content cannot expand the structured resource ceiling or grant execution authority.

When native data is present, both protocols and EvidenceSource collection require `binding.candidate_revision` to equal `contract::candidate_revision(&candidate)`. The helper validates the candidate and returns `sha256:` plus 64 lowercase hexadecimal digits. Hash SHA-256 over `b"semantic-arbiter\0candidate\0"` followed by the complete compact candidate JSON: sort all object keys recursively (including objects inside arrays), preserve array order, and encode scalars with serde_json. Any change to an operation, resource, change order, native name or nested arguments requires recomputing the revision and rebinding evidence. A mismatch is `binding_mismatch`, not a judgment. A malformed name or non-object input is `invalid_input`; input exceeding the byte bound is `message_too_large`. The enclosing 262144-byte message bound and strict JSON recursion bound also apply.

For example, a candidate may include `"native_tool":{"name":"Bash","input":{"command":"printf test","cwd":"/workspace"}}`; these strings are data, not executable instructions or filesystem facts. Full native data reaches model state unchanged. Without native data, serialization omits the field and caller-owned revision labels remain accepted. This content binding does not authenticate a native call or authorize its execution.

### Evidence, gaps and budget

| Field | Meaning and constraint |
|---|---|
| `evidence[].id` | Unique evidence identity referenced by answers and outcomes |
| `evidence[].source_id` | Source identity supplied by the consumer |
| `evidence[].provenance` | `local_verifier`, `source_reported` or `model_echo`; only `local_verifier` qualifies for reconstruction checks |
| `evidence[].binding` | Exact association with the request's task, candidate, instance, workspace and policy |
| `evidence[].captured_at_ms`, `valid_until_ms` | Collection time and exclusive expiry; collection must precede expiry |
| `evidence[].fact.kind` | `rebuild` |
| `evidence[].fact.parameters.resource_id` | Resource compared by the verifier |
| `evidence[].fact.parameters.result` | `identical`: retained recipe reproduced the bytes; `unique`: existing bytes include unreproduced results; `absent`: the resource has no current bytes; `unknown`: comparison was inconclusive |
| `gaps[].source_id`, `scenario` | Explicit missing coverage for a source and scenario; duplicate pairs are invalid. Enabled gaps block `check` but remain visible during eligible `evaluate` fallback |
| `budget.opened_at_ms`, `deadline_ms` | Original event opening and exclusive deadline; deadline must follow opening, and evaluation time cannot precede opening |
| `budget.max_evidence_items`, `max_evidence_bytes` | Caller limits on item count and compact serialized evidence-array bytes, bounded by protocol maxima |
| `check_at_ms` | Check-only evaluation time in the fixture's recorded clock; absent from model requests |

A provenance label is an assertion. Consumers must independently authenticate the task ingress, evidence source and verifier result. Synthetic fixtures only assert these facts. `source_reported` and `model_echo` cannot establish local reconstruction.

`check` uses the recorded clock. Model requests use Unix milliseconds for budget and evidence timestamps. Evidence is usable only when `captured_at_ms <= evaluation_time < valid_until_ms`. In `check`, expired event budgets, exceeded caller evidence limits, or other deterministic unknowns fail closed as candidate `block`. Malformed timestamps, hard size violations and binding mismatches are protocol errors.

### Pre-evaluation evidence collection

`EvidenceSourceRegistry` is an optional producer-side library API, not part of either wire protocol. Consumers explicitly register in-process sources and select source IDs before finalizing a check or evaluation request. Collection receives the frozen request context without evidence or gaps. The registry injects the registered source identity, provenance and request binding; source output cannot replace them. It returns deterministically ordered inline `evidence`, `gaps`, sanitized per-source reports and one final `collected_at_ms`.

Every cooperatively asynchronous source gets one attempt under the minimum of its registered timeout and the original event deadline. Source implementations must return their future promptly and must not block its executor thread; Tokio deadlines cannot preempt blocking in-process code. There is no retry, fallback, implicit source, plugin discovery or deadline renewal. Unavailable, timed-out, future, expired or over-age source batches become explicit gaps for the selected supported scenarios. The final freeze instant rechecks every batch, so time spent collecting a later source cannot leave earlier expired evidence in the result. Invalid source output, duplicate evidence IDs and item or compact-array byte budget excess fail the whole collection without truncation.

Collection does not authenticate a source or promote provenance. The in-memory example remains `source_reported`, so its rebuild claim cannot satisfy the local-verifier requirement. Consumers place the returned evidence and gaps into the request; a deterministic request also sets `check_at_ms` to the returned `collected_at_ms`. `check` and `evaluate` never invoke the registry themselves.

### Local context (`evaluate` only)

Optional `local_context` is a frozen consumer observation, not a policy configuration or an authentication claim. It is omitted when absent and participates in `input_digest` when present. `check` does not accept or use it.

| Field | Constraint and meaning |
|---|---|
| `candidate_revision` | Must equal the request binding |
| `captured_at_ms`, `valid_until_ms` | Ordered timestamps; usable only while `captured_at_ms <= now < valid_until_ms` |
| `files` | At most one observation per candidate resource, no foreign resource IDs |
| `files[].resource_id`, `state` | Candidate resource and `absent`, `regular`, `directory`, `symlink` or `unknown` |
| `files[].delta` | `unchanged`, `changed` or `unknown`; a known delta requires a regular file and digest |
| `files[].existed_at_start` | Boolean or null; incomplete coverage must not imply absence |
| `files[].sha256` | Null or `sha256:` plus 64 hexadecimal digits, only for a regular file |
| `signals` | At most 64 `{kind, resource_id}` hints; resource ID is null or names a candidate resource |
| `gaps` | At most 32 stable identifier-shaped coverage codes |

The closed action vocabulary is defined in [`local_policy.rs`](../../src/local_policy.rs). Signals describe lexical categories such as test/config/dependency changes, external communication, credential-locus reads (`sensitive_read`) and working-tree discards (`working_state_destruction`); they do not prove authorization, malice, shared production use or actual side effects. The core performs no filesystem collection. The [Provider collector](../../../../providers/semantic-arbiter/README.md#local-observations-and-judgment) owns bounded inventory/stat/raw-byte observations; exclusions and limits remain explicit uncertainty.

### Wire bounds

| Limit | Value |
|---|---|
| Contract message | 262144 UTF-8 bytes |
| Candidate changes, evidence items, gap records, IDs per list, obligations | At most 64 each; candidate changes may be empty for a fully attributed read-only call |
| Subject actions | At most one each of `freeze`, `quarantine`, `kill` |
| Identifier | 1–128 ASCII bytes: letters, digits, `.`, `_`, `-`, `:`, `@` |
| Unit confidence | Integer `0..=1000000` |

Contract objects are closed; unknown fields, duplicate fields, unknown enum values and trailing JSON are rejected. Native `input` is the exception to closed field names: arbitrary argument keys are retained, but duplicate keys at every depth (including objects inside arrays and escaped equivalent keys) are rejected before decoding to a JSON value. Unsigned integer fields reject negative or fractional values. Tagged facts and actions use `kind` and `parameters`. Both request protocols, configuration and upstream-reply decoding reject duplicate keys at every depth, including ignored reply metadata. Input JSON formatting is independent of the typed serialization used for digests.

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
| `thresholds.preservation_ppm`, `thresholds.task_scope_ppm`, `thresholds.snapshot_ppm` | Independent integer support thresholds in `500001..=1000000`, each defaulting to `900000` |
| `transport` | Default `https`; `loopback_test` permits HTTP only on literal loopback IP addresses |

The Bailian example uses `https://{WorkspaceId}.cn-beijing.maas.aliyuncs.com/compatible-mode/v1/systemone` and explicitly sets all three thresholds to `800000`. Supply the authorized workspace ID and verify the complete endpoint before use. These thresholds are uncalibrated decision parameters; model accuracy requires separate measurement.

URLs reject embedded credentials, query strings and fragments. HTTPS uses certificate verification. The HTTP adapter is configured to disable redirects, environment proxies, retries and idle connection pooling. `EnvironmentBackend` loads only the configured environment variable when inference is needed and never loads `.env`. Secrets and raw upstream response bodies are excluded from outputs.

## Judgment rules and model exchange

Every unit requires a task and applies declared retention and allowance constraints to every change. Empty resource lists declare no constraint; a preserved resource or a resource outside a non-empty allowance blocks the candidate. `check` remains unchanged: preservation and snapshot require established local-verifier rebuild facts, and missing, inconclusive or gapped evidence fails closed.

`evaluate` distinguishes integrity failures from missing knowledge. Baseline reasons `allowed`, `missing_evidence`, `evidence_gap` and `undetermined` permit local policy or semantic fallback. Preservation and snapshot scan every changed resource for stale, untrusted or conflicting rebuild evidence even when an earlier resource has a gap; these failures remain hard gates. Binding/shape errors remain protocol errors. Task scope needs no rebuild evidence, but its declared coverage gaps remain visible to semantic fallback. Missing facts never become `absent`, permission or proof of reproducibility.

Snapshot and preservation have built-in local policies, tried in the listed order within that unit. A shared completeness fact underlies the read-only policies: the candidate names no change, a `read_only` signal exists and no `opaque_execution` signal exists. Task scope has no local policy: it asks whether a call serves the task goal, which no effect fact settles, so an unrelated or unauthorized read is out of scope even with no write effect and no credential locus. Reads always reach the model; `read_only` and `sensitive_read` are hints for that judgment, not a local gate.

| Unit | `policy_id` | Required facts | Answer |
|---|---|---|---|
| snapshot | `no_loss_read_only` | The read-only completeness fact | `allow` |
| snapshot | `no_current_bytes_lost` | Fresh observations cover every change, every file is currently absent or its exact supported literal edit leaves bytes unchanged, and no `opaque_execution` signal exists | `allow` |
| snapshot | `protect_unique_bytes` | Referenced qualified rebuild evidence contains `unique` | `needs_snapshot` |
| snapshot | `protect_preexisting_bytes` | A fresh observation identifies a regular file as present in the initial inventory, has a stable raw-byte digest, does not establish unchanged bytes, and has no referenced `identical` rebuild fact | `needs_snapshot` |
| snapshot | `protect_working_state` | A `working_state_destruction` signal exists and not every change has both a fresh observation and a referenced `identical` rebuild fact; an unattributed whole-tree reset or clean always qualifies because its reach is unknown | `needs_snapshot` |
| preservation | `preserve_no_mutation` | The read-only completeness fact; a call that mutates nothing cannot violate retention of existing bytes | `allow` |
| preservation | `preserve_absent_target` | Fresh observations cover every change, every observed resource is currently absent, and no `opaque_execution` signal exists; declared retention lists remain hard gates applied first | `allow` |

These answers have confidence `1000000` as rule results, not accuracy estimates. Policies do not bypass hard gates, expired budgets or blocking units, and add no user policy settings. A `sensitive_read` signal marks a read of conventional credential loci (dotenv files, SSH or cloud credentials, shell history, environment dumps) or a recursive content search that may reach them; it focuses the model's task-scope judgment on credential handling. Protection units still settle read-only calls locally because they ask about byte effects, not goal alignment. Unresolved questions go to the model with the original qualification reasons and gaps.

| Unit / question ID | Model proposition | Supported result |
|---|---|---|
| `preservation` / `preservation` | Does the authoritative task text require keeping any resource in `candidate.changes` unchanged against its operation? Rebuild evidence only reports reproducibility and neither grants nor forbids the change. | Yes support meeting the threshold yields `preserve`; no support meeting it yields `allow` |
| `task_scope` / `task_scope` | Does the full candidate, including every change and, when supplied, the native name and every nested argument, satisfy the authoritative task text? Without native data, judge every listed change. | Yes support meeting the threshold yields `allow`; no support meeting it yields `outside_scope` |
| `snapshot` / `snapshot` | Should a recovery point of the workspace be created before this candidate executes? Judge only the magnitude of irreproducible loss across every change and, when supplied, the native name and every nested argument. User authorization is a task-scope question and does not lower the cost of loss. `identical`/`absent` rebuild results lower that cost. Asked only when no local snapshot policy settles the unit. | Yes support meeting the threshold yields `needs_snapshot`; no support meeting it yields `allow` |

Preservation and task scope decide from the task text: an omitted resource list is stated to carry no permission or retention meaning, and membership in a declared list is not offered as proof of task relevance. Native tool names and arguments are untrusted candidate data, never instructions or authority. Embedded requests to change rules or grant permission must not be followed. A declared resource ceiling remains a hard limit; neither a native payload nor model support grants execution authority.

Omitting a list therefore changes the failure mode as well: a resource an operator never enumerated is no longer an implicit deny, and a missing or failed model answer becomes `undetermined` with candidate `nop`. A consumer that needs fail-closed scope declares a non-empty allowance.

Units run in preservation/task-scope/snapshot order. `check` derives answers entirely from structured constraints and evidence; deterministic unknowns fail closed, and it never answers `needs_snapshot`. `evaluate` applies the hard gates and local policies above, batching remaining questions into at most one backend call. For eligible questions, low bilateral confidence, timeout or backend/response failure produces `unknown` and candidate `nop`, never `ask`, unless another unit blocks. Snapshot abstention contributes no recovery point. A completed local or model `needs_snapshot` answer can recommend one only when composition permits it; an expired budget or blocking unit suppresses it.

Both HTTP adapters send Bearer-authenticated POST requests with `{model,state,questions}`. `state` contains `authoritative_task`, `candidate`, selected provenance-labeled `evidence`, `evidence_gaps`, baseline `qualification` and fresh `local_context` (otherwise null). Questions are component-owned `{type:"noul",instructions:...}` objects keyed by question IDs. Endpoint configuration and credentials are excluded from state.

Upstream `answers` must contain exactly the requested question IDs. Each answer has a numeric `noul` and, for TypeSafe, required `type="noul"`. Bailian permits the type to be omitted; a supplied value must be `"noul"`. Optional top-level `model`, `usage`, `request_id` and `latency_ms` are accepted as ignored metadata. Unknown fields, duplicate keys, extra/missing answers and malformed probabilities become sanitized model-response errors: eligible units answer `unknown` and candidate control is `nop` unless the deterministic hard gate already requires `block`. Returned model metadata cannot establish backend identity.

Only Noul answers are supported. Each value represents an independent P(yes) for one proposition; probabilities across questions need not sum to one. Values must be finite JSON numbers in `[0,1]`. Strings, booleans, nulls and out-of-range numbers are rejected. Normalization preserves the exact decimal token, including exponent notation, and computes `yes_ppm=floor(p*1000000)` and `no_ppm=floor((1-p)*1000000)` before floating-point conversion. Threshold comparisons use these exact floors. `raw_json_number()` provides lossless access; `raw()` is a lossy convenience accessor. Probability and unit confidence express model support, with no established calibration to factual correctness or safety.

### Judgment-unit registry

`JudgmentUnitRegistry` is an explicit process-local replacement point for the three closed `Scenario` values. Its constructor requires exactly one `preservation`, one `task_scope` and one `snapshot` unit, with nonempty distinct question IDs. The registry cannot add an arbitrary wire scenario and performs no plugin or dynamic code discovery. A consumer replacing one unit supplies the built-in or another reviewed implementation for the other slots.

`Evaluator::with_units` uses the supplied registry while preparing questions and deriving the response. Each unit's question ID and question text enter `config_digest`. Once evaluation stabilizes, `EvaluationRecord` freezes the expected response and the original configuration digest; later response validation compares those immutable values and does not invoke a unit or model again. Central answer validation requires the original unit and evidence references, bounds confidence to `0..=1000000`, and permits only the value/reason pairs defined for that closed scenario. A custom unit returning any other answer causes `invalid_response`.

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

A unit answer describes that unit's conclusion; it is not itself CandidateControl. Candidate disposition is the aggregate `outcome.control.candidate`. For `check`, any `preserve`, `outside_scope` or deterministic `unknown` answer produces `block/[audit]`, otherwise `allow/[audit]`; `check` never emits `snapshot`. For `evaluate`, deterministic hard blockers remain `block/[audit]`; when an eligible model question abstains through low bilateral confidence or failure, its answer is `unknown` and the current producer returns candidate `nop/[audit]`. Completed, nonblocking local or model judgments produce `allow`, and `allow/[snapshot,audit]` when the snapshot unit answers `needs_snapshot`. A `preserve` or `outside_scope` answer blocks the candidate and suppresses the snapshot recommendation, so a resource the task requires keeping is blocked rather than snapshotted and modified. Deterministic confidence of `1000000` records a completed rule result under asserted inputs and provides no provenance authentication.

An `allow` remains advisory. When it includes `snapshot`, the consumer must hold the candidate, independently authorize and confirm the snapshot, and revalidate binding, evidence and deadline before execution. Successful inference, approval or response delivery cannot replace execution authorization.

### Model-evaluation response additions

| Field | Meaning |
|---|---|
| `config_digest` | Digest of nonsecret configuration plus question IDs and texts; distinct from the caller's `config_revision` label |
| `backend.kind`, `backend.mode`, `backend.model` | Constructed adapter kind (`bailian`, `typesafe`, `fixture`), mode (`remote`, `loopback_test`, `fixture`) and configured model name/alias |
| `evaluated_at_ms` | Final validity-check time in Unix milliseconds |
| `elapsed_ms` | Monotonic elapsed time through evaluation, including input preparation |
| `model_called` | Whether the selected backend was invoked; false when no eligible question is called |
| `judgments[].unit`, `question_id` | Unit identity and the question identity bound to its exact text |
| `judgments[].threshold_ppm` | Configured support threshold for that unit |
| `judgments[].probability` | Validated `{raw,yes_ppm,no_ppm}`, or null when unavailable |
| `judgments[].error` | Sanitized backend error or null; examples include `timeout`, `authentication`, `rate_limited`, `transport`, `http`, `invalid_response`, `response_too_large` |
| `judgments[].model_used` | A validated probability was used in deriving the final unit answer; false after expiry or a deterministic rejection |
| `judgments[].evidence_refs` | Locally selected evidence references for the unit |
| `judgments[].route` | `hard_gate`, `local_policy`, `model_fallback`, `observation_expired` or `budget_exhausted` |
| `judgments[].policy_id` | Stable local policy ID, or null |
| `judgments[].baseline_reason` | Evidence qualification reason retained before semantic normalization |

The CLI captures wall and monotonic clocks at ingress. Preparation, inference, body reads, validation and composition share the original deadline and configured cap. Evidence and observations are rechecked after inference/composition. Expiry of initially fresh observations prevents adoption of dependent local/model judgments (`observation_expired`); qualified unique rebuild evidence can still independently settle snapshot. Budget exhaustion on an eligible path yields `unknown`/`nop` unless a hard blocker applies. A probability may remain with `model_used=false`. `model_called` records backend invocation, not successful HTTP transmission: lazy credential failure can set it true without any network request.

Retain `EvaluationRun::record()` independently. `evaluation::decode_response` and `validate_response` require the original request, configuration and non-deserializable local `EvaluationRecord`, then compare against the frozen expected response without invoking a unit or model again. Backend injection is a trusted in-process API; callers must bind the actual adapter to its configuration. Wire traces and backend labels alone provide no authentication across an untrusted process boundary. Record validation establishes recorded consistency; consumers must check freshness again before execution. Check response validation recomputes the deterministic result from the retained request.

### Digests

Digests are `sha256:` followed by lowercase SHA-256 over a domain separator and compact, declaration-order typed JSON. Arrays, enum values and nulls are preserved. Cross-language implementations must match this serialization and the repository's digest vectors; hashing incoming whitespace or using RFC 8785 produces a different contract.

| Digest | Domain separator | Typed payload |
|---|---|---|
| Check `input_digest` | `semantic-arbiter\0check-input\0` | `CheckRequest` |
| Evaluation `input_digest` | `semantic-arbiter\0evaluation-input\0` | `EvaluationRequest` |
| Evaluation `config_digest` | `semantic-arbiter\0evaluation-config\0` | Pair of configuration and the scenario-ordered scenario/question ID/question definitions |
| `ModelConfig::digest()` | `semantic-arbiter\0model-config\0` | Configuration alone; separate from the response's `config_digest` |

`\0` denotes a zero byte. `config_revision` is a caller-maintained policy label; `config_digest` binds effective configuration and question semantics. Credential values are absent from these payloads. Digests bind content and provide no sender authentication.

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
| `api_version` | `semantic-arbiter/v1alpha1` |
| `request_id`, `binding` | Exact retained request association |
| `directive_id` | ID of the immutable directive retained under `(request_id,binding,directive_id)` |
| `state`, `reason` | Only `applied/confirmed`, `rejected/denied`, `unknown/unconfirmed` are accepted |

Consumers create receipts after independent admission and effect verification. `decode_receipt` takes a `CheckRequest` and retained Outcome and checks correlation and state/reason consistency. This API supplies no model-evaluation receipt decoder or operating-system effect verification. Consumers must separately verify capability, identity, scope, authorization, idempotency and deadline. An answer `unknown`, candidate `nop`, candidate `ask`, and receipt `unknown` are four distinct states: incomplete semantic support, no candidate judgment, a deterministic approval requirement, and an unconfirmed execution effect. For a receipt `unknown`, query the original receipt or inspect the effect manually; never automatically repeat the directive. Rejection also grants no retry permission. Neither operation manufactures execution receipts.

### Consumer-side executor lifecycle

The `executor` module is a consumer-side library contract. `AuthorizationContext` is trusted input created by the consumer; evidence, model output and a Directive cannot create or expand it. Admission requires the protocol version `semantic-arbiter/v1alpha1`, the exact granular `DirectiveCapability`, an unexpired deadline, matching candidate/workspace/instance bindings and an allowlist containing every nonempty controlled scope or parameter ID. `ExecutorRegistry` registers exactly one Executor per capability.

`idempotency_key` is a stable domain-separated digest of `api_version`, `request_id`, the complete `Binding` and the complete immutable `Directive`. Execution follows claim, conditional dispatch and query. A new claim dispatches at most once and then queries the effect. An existing claim skips dispatch and queries the original request. A dispatch result never determines the receipt; only the independent query maps to `applied/confirmed` or `rejected/denied`. A failed or unconfirmed query maps to `unknown/unconfirmed`, and an unknown effect is never automatically replayed.

`executor::fixture` supplies an in-memory claim store and scripted Executor solely for tests and examples. It performs no external action and establishes no native Adapter support. Real Executors, native Adapter adoption, durable production claims, persistent audit and operating-system effect verification remain pending consumer integration.

## CLI completion and integration boundary

A complete valid judgment exits `0`, including model ambiguity, inference failure or missing/unusable credentials yielding `unknown`/`nop`, and any valid `block`. `EnvironmentBackend` validates configuration first but reads credentials only inside inference; purely local evaluation needs none. Missing credentials are `missing_credentials` in the judgment trace, not a CLI construction error. Hard blockers still take precedence. Malformed/missing request or configuration, invalid bindings and input/I/O failures exit `2` with sanitized stderr JSON. Allowance output is bounded by the original deadline, configured cap, adopted evidence and dependent local-context expiry; expiration exits `2` with `deadline_exceeded`. Discard partial stdout. Accept only a complete zero-exit response, inspect candidate control and revalidate before use; zero is not permission.

The separate [AW shadow Provider](../../../../providers/semantic-arbiter/README.md) uses `aw-provider/v1alpha1`; this CLI cannot serve as that Provider. Private configuration contains resource constraints, not task text or a preset session. `input.submit` persists per-session prompts and a bounded initial inventory in `task.json`; `tool.before` evaluates complete native calls against the captured turns. Session identity hashes `[adapter, binding_id, instance_id, native_session_id]`; the joined task text determines `task_version`. Invalid context, missing/oversized tasks and invalid/oversized candidates remain uncovered. Unmapped or `no_mutation` object inputs retain the full call as an opaque candidate instead of being skipped.

The Provider freezes local context and Git evidence/gaps under the original deadline, recording call stages in `calls.jsonl`. Its [README](../../../../providers/semantic-arbiter/README.md#records-bounds-and-cleanup) owns storage, collection limits and operating instructions. Bounded observation does not neutralize the existing Git verifier's clean filters: use a trusted controlled workspace and reviewed Git configuration, not an assumed no-execution boundary.

Valid Provider invocations return the original AW digest, `status: ok` and `effects: []`, including semantic failures. AW and consumer integrations own execution and effect verification; this Provider does not execute snapshots, blocking, rollback or cleanup. Fixture probabilities establish contracts, not model efficacy or native adoption.
