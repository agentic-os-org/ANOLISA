# semantic-arbiter

[中文版](../../zh/semantic-arbiter/index.md)

Evaluate preservation, task-scope and snapshot questions with a configured decision model, or deterministically check frozen reference inputs at a recorded clock. `semantic-arbiter` returns typed recommendations without starting an Agent, replaying Agent actions or modifying a workspace. Consumers remain responsible for authorization and execution.

## Target design and availability

The complete target design covers six business scenarios: protect results, enforce task scope, supply context, diagnose problems, respond to security events and govern recovery resources. `semantic-arbiter` never executes actions, including in the target architecture. All Executors permanently belong to consumer integrations.

| Status | What it means in this guide |
|---|---|
| **Implemented core** | Preservation/task-scope/snapshot judgments, frozen inline evidence, bounded EvidenceSource collection, closed-scenario JudgmentUnit replacement, deterministic Outcome composition, consumer-side executor contracts, deterministic `check` and Noul model evaluation through Bailian/TypeSafe |
| **Contract types/validation only** | Full `nop < allow < ask < block` CandidateControl vocabulary, wider Outcome types, Directive projection and receipt validation exist; current scenarios do not produce `ask` or wider actions |
| **AW shadow Provider** | Captures session prompts through `input.submit`, then evaluates complete native candidates at `tool.before` with bounded local observations and semantic fallback; effects remain empty and an advisory decision is surfaced under shadow without enforcement |
| **Pending consumer integration** | Complete native event/history delivery, admission and blocking, real Executors, native adoption verification, persistent production claims/audit and effect verification still require integration |
| **External capability requires verification** | AgentSight, AW, ws-ckpt and native Adapter interfaces must be checked against target-version source and deployment behavior; historical capability inventories establish no support |

| Business scenario | Purpose | Proposed output | Availability |
|---|---|---|---|
| Preservation | Identify changes the task requires keeping | Advisory `block` for retention or integrity rejection; eligible semantic judgment can allow, block or abstain (`nop`) | Implemented core: `preservation`; missing rebuild knowledge permits semantic fallback |
| Snapshot | Recommend a recovery point before modification | Local policies or model judgment can produce `[snapshot,audit]`; execution is not performed | Implemented core: `snapshot`; `check` never recommends one; execution pending |
| Task scope | Compare the full call with task text and resource ceilings | Advisory `nop/allow/block` with audit; no authorization grant | Implemented core: `task_scope`; Provider retains precise multi-resource or opaque full-native candidates |
| Context supplementation | Find missing skills, memories or other reviewed material | `retrieve_skill/retrieve_memory`, `inject_context`, `audit`; candidate handling depends on delivery path | Pending scenario and integration |
| Diagnosis | Assess failures, resource pressure and task progress; request assistance | `diagnose`, `notify`, `audit`; a later candidate may need `ask` | Pending scenario and integration |
| Security response | Assess event credibility and contain the affected subject | `block` before execution; `freeze/quarantine/kill` with `audit`, optionally `notify` | Pending scenario and integration |
| Cleanup and governance | Retain audit and recovery obligations while reclaiming eligible checkpoints | `nop` with `audit/cleanup`; recovery may suggest `rollback` | Pending scenario and integration |

Only `preservation`, `task_scope` and `snapshot` are callable scenario names. Other rows describe planned behavior, not accepted request values.

### From evidence to a decision

The target flow is **EvidenceSource → JudgmentUnit/Scenario → validated answers → Outcome → consumer admission → Executor → Receipt**.

| Stage | Responsibility and boundary |
|---|---|
| Consumer and EvidenceSource | Authenticate task/candidate origins, bind identities and versions, and freeze bounded evidence. `EvidenceSourceRegistry` explicitly registers process-local sources, enforces freshness/size/deadline bounds and returns inline evidence plus gaps |
| JudgmentUnit and Scenario | A unit selects required evidence, a question and its threshold. `JudgmentUnitRegistry` contains exactly one unit for each existing closed scenario: `preservation`, `task_scope` and `snapshot` |
| Validated answers and Outcome | Validate answer shape, evidence references, coverage and expiry, then deterministically map answers to restrictions and obligations. `composition::compose_outcome` provides deterministic public composition |
| Consumer admission and Executors | Use trusted consumer authorization, granular capabilities, explicit Executor registration and independent effect queries; native Adapter support and real action implementations require consumer integration |
| Receipt and audit | Retain the immutable directive, verify its effect and record each result. Sending a request or receiving a successful transport response proves no effect |

The scenario registry cannot add arbitrary wire scenarios and performs no dynamic discovery. `Evaluator::with_units` binds each unit's declared revision, question ID and question content into the configuration digest while deriving the response. The retained evaluation record freezes the expected response, so later validation does not invoke a custom unit again. Central validation rejects custom answers that change the unit or evidence references, exceed the confidence bound, or use a value/reason pair outside the closed scenario.

Consumer integration must capture task text and retention requirements from an authenticated user/operator entry, retain the current task and version independently of bounded session history, and mark omitted or truncated history. Freeze the candidate, workspace, task and configuration revisions together. Collection, inference, protection and final revalidation share the original event deadline; auxiliary collection never renews it. Changed task, candidate or policy revisions require a fresh judgment.

AgentSight is intended to supply auxiliary history, observations, trends and forensic reports. Missing reports cannot mean permission, and projecting `source_reported` through a consumer cannot promote it to user authorization or `local_verifier`. Local reconstruction requires comparing bytes rebuilt from retained generators and old inputs; the consumer supplies and authenticates that result. The core neither runs generators nor checks disk contents. Workspace identity and resource scope require independent verification; cwd alone proves neither containment nor permission.

## Build from source

Installer and RPM distribution are unavailable. Use a Linux checkout with Rust/Cargo, rustfmt, clippy and Make. From the repository root:

```bash
cd src/semantic-arbiter
make build
./target/debug/semantic-arbiter describe
```

Developer examples exercise replacement of one unit in the closed scenario registry, the in-memory executor fixture and in-memory EvidenceSource collection:

```bash
cargo run --example judgment_unit
cargo run --example executor_fixture
cargo run --example evidence_source
```

`executor_fixture` performs no external action and demonstrates no native Adapter support. `describe` lists `deterministic_check` and `model_evaluation`, the `bailian` and `typesafe` backends, the `preservation`, `task_scope` and `snapshot` scenarios, and the message size limit. Current `check` output uses candidate `allow/block`; current `evaluate` output uses `nop/allow/block`. Both produce only advisory `snapshot/audit` obligations. No daemon is required.

## Command help

Running without arguments, or with `help`, `-h` or `--help`, prints human-readable command descriptions, scenarios, examples and exit statuses. Command-specific help is available through `help <command>` or `<command> --help` / `-h`. Help requires no request, configuration or credentials and makes no model calls.

```bash
./target/debug/semantic-arbiter --help
./target/debug/semantic-arbiter help describe
./target/debug/semantic-arbiter check --help
./target/debug/semantic-arbiter evaluate --help
```

Help exits `0` with text on stdout. `describe`, `check` and `evaluate` retain JSON output; invalid arguments retain sanitized JSON errors on stderr with exit `2`.

## Evaluate with a decision model

When semantic fallback is needed, `evaluate` sends task text, the full candidate, selected provenance-labeled evidence, qualification reasons, gaps and fresh local context to the configured service. Authorize the endpoint, credential use and outbound data before running it. Use the supplied synthetic samples first. Remote availability and semantic accuracy require separate validation.

### Complete native candidates

A consumer may include `candidate.native_tool` with a nonblank `name` (at most 128 UTF-8 bytes) and the complete `input` JSON object (at most 65536 compact JSON bytes). The whole request remains bounded to 262144 bytes. Inputs must not be truncated to fit these limits. Native names and arguments are untrusted data; they grant no authority and do not replace a declared resource ceiling. The `task_scope` question considers every listed change and every supplied argument.

When `native_tool` is present, `binding.candidate_revision` must equal the public Rust `contract::candidate_revision(&candidate)` result. It hashes the complete candidate using recursively sorted object keys, compact JSON and the domain `semantic-arbiter\0candidate\0`. Changes to any listed change, their order, the tool name or parameters require a fresh revision and matching evidence bindings. Both judgment commands reject nested duplicate JSON keys. Requests without native data accept a caller-owned candidate revision label.

### Configure the backend

Use `src/semantic-arbiter/examples/evaluation/bailian.json` or `src/semantic-arbiter/examples/evaluation/typesafe.json` from the repository root. The Bailian example supplies the workspace endpoint template `https://{WorkspaceId}.cn-beijing.maas.aliyuncs.com/compatible-mode/v1/systemone`. Set `{WorkspaceId}` to your authorized workspace ID and verify the complete URL; the adapter uses it unchanged. TypeSafe defaults to `https://api.typesafe.ai` and appends `/v1/systemone` after any configured gateway prefix.

| Field | Meaning |
|---|---|
| `config_revision` | Policy label matching request `binding.config_revision` and all evidence bindings; examples use `policy-v1` |
| `backend.kind` | `bailian` or `typesafe`; configuration cannot select a fixture backend |
| `backend.endpoint_url` / `backend.base_url` | Complete Bailian inference URL / TypeSafe base URL |
| `backend.model` | Defaults to `decision-model-preview` / `jev-latest`, respectively |
| `backend.key_env` | Environment-variable name; defaults to `DASHSCOPE_API_KEY` / `TYPESAFE_API_KEY` |
| `timeout_ms` | Required, `1..=120000`; preparation and evaluation share this cap from ingress, within the original request deadline |
| `max_response_bytes` | Required, `1..=262144`; enforced while streaming the upstream body |
| `thresholds.preservation_ppm`, `thresholds.task_scope_ppm`, `thresholds.snapshot_ppm` | Independent support thresholds, `500001..=1000000`; each defaults to `900000` |
| `transport` | Default `https`; `loopback_test` permits HTTP only on literal loopback IPs and labels responses as testing |

The Bailian example explicitly uses `800000` for all three thresholds; the TypeSafe example uses `900000`. These values have no established accuracy calibration. The response's `config_digest` binds effective configuration and question semantics; `config_revision` remains the caller-maintained policy label.

HTTPS verifies certificates. URLs reject embedded credentials, query strings and fragments. The adapter disables redirects, environment proxies and retries. It loads no `.env` file. Keep credential values out of JSON and CLI arguments; `key_env` contains only the variable name.

### Run a synthetic request

Run only the backend you authorized, after verifying its endpoint configuration. These Bash commands prompt without echoing the key or placing its value in shell history. Python 3 is used only by the synthetic request generator; the Rust library and CLI need no Python SDK.

For Bailian:

```bash
read -rsp 'API key: ' DASHSCOPE_API_KEY; printf '\n'
export DASHSCOPE_API_KEY
python3 examples/evaluation/request.py preservation-allow | ./target/debug/semantic-arbiter evaluate --config examples/evaluation/bailian.json -
status=${PIPESTATUS[1]}
unset DASHSCOPE_API_KEY
printf 'semantic-arbiter exit: %s\n' "$status"
```

For TypeSafe:

```bash
read -rsp 'API key: ' TYPESAFE_API_KEY; printf '\n'
export TYPESAFE_API_KEY
python3 examples/evaluation/request.py preservation-allow | ./target/debug/semantic-arbiter evaluate --config examples/evaluation/typesafe.json -
status=${PIPESTATUS[1]}
unset TYPESAFE_API_KEY
printf 'semantic-arbiter exit: %s\n' "$status"
```

The generator accepts `preservation-allow`, `task-scope-allow`, `snapshot`, `preserve` and `missing-evidence`. It emits a fresh 30-second Unix-millisecond budget and synthetic evidence timestamps. Never refresh real evidence timestamps this way. Evaluate an already frozen request file with:

```bash
./target/debug/semantic-arbiter evaluate --config examples/evaluation/bailian.json request.json
```

This command retains the request's deadline and uses its inline evidence. It collects no new evidence. Task origins and provenance labels require independent authentication; writing `local_verifier` in JSON does not verify a reconstruction result.

### Interpret the result

| Scenario | Question and decision |
|---|---|
| `preservation` | Does the authoritative task text require keeping the resource unchanged? Yes support meeting the threshold yields `preserve`; no support meeting it yields `allow`. Rebuild evidence is data the model may weigh, never a floor |
| `task_scope` | Does the full candidate satisfy the authoritative task text? Yes support meeting the threshold yields `allow`; no support meeting it yields `outside_scope` |
| `snapshot` | Local policies first settle no-loss or protection cases. Otherwise the model judges irreproducible loss: yes support meeting the threshold yields `needs_snapshot`; no support yields `allow`. Task authorization does not lower loss cost |

`evaluate` keeps missing tasks, declared retention/allowance limits and stale, untrusted or conflicting rebuild evidence as hard gates. Missing/unknown evidence and coverage gaps remain visible but allow semantic fallback, including preservation and task scope; they never establish absence or permission. Declare a non-empty allowance when you need a hard resource ceiling.

Optional `local_context` supplies candidate-bound observations. Snapshot and preservation try built-in local policies before any model question; task scope has none, because goal alignment is semantic and an unrelated read is out of scope even without a write effect. Snapshot: `no_loss_read_only` allows a fully attributed read-only call; `no_current_bytes_lost` allows when every observed change has no current bytes to lose or leaves exact bytes unchanged and no opaque execution is present; `protect_unique_bytes` recommends a snapshot for qualified unique rebuild evidence; `protect_preexisting_bytes` protects a stably read preexisting regular file without an unchanged delta or qualified identical rebuild fact; `protect_working_state` recommends a snapshot for a named working-tree discard unless every named target is provably identical. Preservation: `preserve_no_mutation` allows a read-only call; `preserve_absent_target` allows creating or removing resources with no current bytes. Task scope always asks the model; a `sensitive_read` signal (credential-locus reads or a recursive content search) focuses that judgment on credential handling. Exact conditions and bounds are in the [protocol reference](../../../../src/semantic-arbiter/docs/design/protocol.md#judgment-rules-and-model-exchange). These policies add no user settings and do not grant task authorization. Unresolved questions share at most one backend call; entirely local decisions need no credentials. `check` remains unchanged and fails closed on deterministic unknowns.

The CLI uses built-in units and fixed composition. Library consumers may use `Evaluator::with_units` for the closed scenario registry; no additional wire scenario is introduced.

Inspect these response fields:

| Field | What to check |
|---|---|
| `answers` | Per-unit conclusion, reason, confidence and evidence references |
| `outcome.control.candidate` | Aggregate candidate disposition: current evaluation can produce `nop`, `allow` or `block` |
| `outcome.obligations` | Required advisory `audit`, plus `snapshot` only after a completed snapshot judgment answers `needs_snapshot` and no unit blocks |
| `judgments` | Question/unit identity, threshold, probabilities, `model_used`, errors, `route`, local `policy_id` and original qualification `baseline_reason` |
| `backend`, `model_called` | Constructed adapter identity and whether it was invoked |
| `input_digest`, `config_digest`, `binding` | Association with the retained request and configuration |
| `evaluated_at_ms`, `elapsed_ms` | Final check time and elapsed preparation/evaluation time |

Only Noul model answers are supported. Each probability represents P(yes) for one proposition. Yes/no support is conservatively normalized in integer millionths from exact decimal tokens; confidence has no established calibration to factual correctness. Upstream `model`, `usage`, `request_id` and `latency_ms` are accepted as ignored metadata. Unknown fields, duplicate keys, mismatched questions and malformed probabilities become model-response errors, yielding an `unknown` answer and candidate `nop` unless a deterministic hard blocker already applies.

Unit answers and candidate disposition have separate meanings. Deterministic hard blockers remain candidate `block`. For a model-eligible question, low bilateral confidence, timeout, or backend/model-response error yields an `unknown` answer and candidate `nop`, never `ask`; there is no fixture fallback. Snapshot model abstention retains `[audit]` only; a `preserve` or `outside_scope` answer blocks the candidate and suppresses the snapshot recommendation. Evidence validity is rechecked after inference and composition.

An `allow` with `[snapshot,audit]` requires the consumer to hold the candidate, independently authorize and confirm the snapshot, and revalidate before execution. Ordinary `allow` and candidate `nop` also grant no execution authority. Responses contain no execution receipts, secrets or raw upstream bodies. Library response verification requires the original request, configuration and independently retained `EvaluationRecord`; exported traces cannot authenticate themselves.

## Check frozen reference cases

The samples contain synthetic constraints and asserted evidence. `check` deterministically evaluates the frozen inputs with their recorded `check_at_ms`, makes no model call, does not interpret task text, and does not replay an Agent or actions:

```bash
./target/debug/semantic-arbiter check examples/preservation-allow.json
./target/debug/semantic-arbiter check examples/preserve.json
./target/debug/semantic-arbiter check examples/snapshot.json
./target/debug/semantic-arbiter check examples/missing-evidence.json
./target/debug/semantic-arbiter check - < examples/task-scope-allow.json
```

Expected candidate/obligation combinations are, in order, `allow/[audit]`, `block/[audit]`, `allow/[audit]`, `block/[audit]` and `allow/[audit]`: the `snapshot` sample's unique bytes are an established fact; `check` never recommends a snapshot, while `evaluate` turns the same input into a `needs_snapshot` answer without a model question. `examples/index.json` records expectations. Deterministic unknown fails closed. Both use `semantic-arbiter/v1alpha1`: `check` with mode `deterministic_check`, model evaluation with mode `model_evaluation` and real-time timestamps. Pass check samples through the synthetic generator before model evaluation.

## Outcome design reference

Outcome separates candidate restrictions, persistent subject requests and ordered obligations, with `evidence_refs` identifying adopted evidence. The full CandidateControl interface applies to every operation and future scenario; the broader vocabulary below is **contract types/validation only** where noted. Current `check` output is limited to candidate `allow/block`; current `evaluate` output is limited to `nop/allow/block`. Both have empty `control.subject[]` and only `snapshot/audit` suggestions. All execution remains pending consumer integration.

### Candidate control

| Value, in increasing strictness | Meaning |
|---|---|
| `nop` | No candidate judgment formed, including eligible semantic abstention; audit the gap. It grants no permission and cannot confirm required protection |
| `allow` | Judgment completed without objection; grants no execution authority |
| `ask` | A deterministic decision that this candidate requires human approval; hold it while waiting and on timeout. Approval requires fresh context checks and proves neither authorization nor execution |
| `block` | Prevent this candidate through the consumer's native blocking interface; cannot undo an operation already executed |

`composition::compose_outcome` takes the maximum result under `nop < allow < ask < block`; a later enhancement cannot cancel a block. Candidate control lasts for one candidate. `ask` is never inferred from ambiguity: its types, validation and Approval Directive projection are reserved now, while current scenarios do not produce it and real approval execution plus Adapter admission remain pending consumer integration. An `unknown` unit answer means insufficient semantic support. Deterministic `check` maps such protection uncertainty to `block`; an eligible model abstention in `evaluate` produces candidate `nop` unless a deterministic hard blocker already requires `block`. An unknown execution receipt is separate again: it means an effect was not confirmed.

### Subject control

| `action` | Effect sought | Lifecycle and authorization |
|---|---|---|
| `freeze` | Suspend execution while retaining state | Persistent until separately authorized resumption; does not vacate cwd |
| `quarantine` | Restrict access to network, sensitive paths or credentials | Independent of suspension; lifting restrictions requires new authorization |
| `kill` | Terminate the subject | Irreversible; requires stable target identity, scope checks and human confirmation |

Each `SubjectDirective` carries `id`, bound `instance_id`, `action` and nonempty consumer-resolved `scope_ids`. A bare PID is insufficient. `compose_outcome` forms a bounded union with at most one exact request for each of freeze, quarantine and kill. Exact duplicates collapse; conflicting requests for one action, ID conflicts, binding mismatches and unavailable granular capabilities are rejected. The result is sorted as freeze, quarantine and kill. Requests describe desired actions, not confirmed process state; each needs its own authorization and receipt.

### Obligations

Each obligation has an outcome-local unique `id`, `critical` and an `action` with closed `kind/parameters`. Consumers resolve controlled IDs and check access scope; parameters contain no arbitrary commands, scripts or generated executable text.

| `action.kind` | Controlled `parameters` | Purpose / class |
|---|---|---|
| `audit` | `{}` | Record sanitized judgment and execution metadata / protection |
| `snapshot` | `workspace_id` | Save a recovery point before execution / protection |
| `rollback` | `workspace_id`, `checkpoint_id` | Suggest restoration to an authorized checkpoint / protection |
| `cleanup` | `workspace_id`, nonempty `checkpoint_ids` | Reclaim authorized, no-longer-needed checkpoints / protection |
| `notify` | `recipient_id` | Notify an authorized recipient / enhancement |
| `diagnose` | `diagnostic_id` | Run a registered, bounded diagnostic / enhancement |
| `retrieve_skill` | `query_id` | Retrieve skills within authorized scope / enhancement |
| `retrieve_memory` | `query_id` | Retrieve memories within authorized scope / enhancement |
| `inject_context` | Nonempty reviewed `material_ids` | Supply material through an admitted native channel / enhancement |

`compose_outcome` exact-deduplicates obligations by ID and rejects different content under the same ID. It requires declared granular capabilities and matching bindings, sorts obligations by action class and ID, sorts evidence references, and produces exactly one `audit`; when authorized, a missing audit is filled with the canonical critical `audit`, while multiple distinct audit requests fail. Protection always fails closed: unconfirmed required work holds dependent execution, even with `critical=false`. Enhancements normally fail open with visible failures; `critical=true` upgrades them to protection semantics.

### Directives and receipts

`Outcome::directives` projects `ask`, subject requests and obligations without execution, scheduling or merging. `allow/nop` have no action; `block` uses native consumer denial. Associate each retained immutable directive and receipt by **`request_id + binding + directive_id`**, never by action name alone.

| Receipt `state/reason` | Meaning |
|---|---|
| `applied/confirmed` | Target action independently verified as effective |
| `rejected/denied` | Target action confirmed not to have taken effect; no retry permission implied |
| `unknown/unconfirmed` | Effect cannot be determined; query the original receipt or verify manually before further action |

An unknown receipt concerns execution, unlike an unknown judgment answer, candidate `nop`, or candidate `ask`. Never automatically repeat a directive with an unknown effect or release a candidate depending on it. Current `decode_receipt` validates check-protocol correlation and state/reason consistency using `CheckRequest` and the retained Outcome; it supplies neither a model-evaluation receipt decoder nor an OS-effect checker. Neither operation produces execution receipts.

The public `executor` module supplies consumer-side admission and lifecycle contracts. A trusted `AuthorizationContext` carries a supported protocol version, the immutable request/binding, expiry, granular capabilities and allowed nonempty controlled IDs. The stable idempotency key binds `api_version + request_id + Binding + Directive`. `ExecutorRegistry::execute` admits, claims, dispatches only a new claim and always queries the original immutable request. An existing claim is queried without another dispatch. The dispatch result does not determine whether an effect was applied; an independent query determines the receipt. A failed or unconfirmed query becomes `unknown/unconfirmed`, which is never replayed automatically.

## Consumer integration design

**The Executor trait and registry are implemented; every real effecting Executor below remains pending consumer integration and outside semantic-arbiter.** Admission must intersect configured permission, declared outputs, Executor support and verified native Adapter capabilities, then check identity, scope and deadline. Unsupported protection keeps the candidate blocked. Consumers own scheduling, quotas, durable production claims, persistent audit and effect verification. `executor::fixture` is limited to in-memory tests/examples and proves no native Adapter support.

| Responsibility | Executor | Required behavior |
|---|---|---|
| Recovery resources | `WsCkptExecutor` | Handle `snapshot/rollback/cleanup` through verified checkpoint interfaces and their distinct preconditions |
| Accountability | `AuditExecutor` | Record judgments, coverage gaps and every obligation/subject result; expose write failures; grant no permission |
| Human communication | `NotificationExecutor` | Resolve authorized recipients, rate-limit and deduplicate; unknown delivery remains unconfirmed |
| Human decisions | `ApprovalExecutor` | Correlate question/reply with the held candidate; enforce timeout and revalidate before resumption |
| Diagnostic evidence | `DiagnosticExecutor` | Run only registered diagnostics with execution/output limits; retain source labels and suppress repeated event/target alerts |
| Skill evidence | `SkillRetrieverExecutor` | Query authorized skills; bind query, version, scope and cache validity; review results before injection |
| Memory evidence | `MemoryRetrieverExecutor` | Query authorized memory with the same version, scope, cache and review controls |
| Context delivery | `ContextInjectorExecutor` | Check reviewed material, task association, native adoption and byte limits |
| Subject protection | `SubjectControlExecutor` | Resolve stable target instances, authorize each action/scope and verify each effect independently |

Planned lifecycle points include `tool.before` for protection, `permission.request` and `compact.before` for approval, `tool.after`/`runtime.observed`/`turn.stop` for diagnosis, `security.violation` for response, and `turn.stop`/`session.end` for recovery and cleanup. These placements do not establish current `invoke` support; diagnosis requiring a later approval must bind it to that later candidate's pre-execution event.

Before enabling any integration, maintain a verified Adapter matrix covering admitted events, native blocking/approval/injection channels, target identity, workspace scope, effective deadlines, size limits and receipt/effect verification. A schema or `describe` enum alone proves no native capability. Verify target source and actual adoption; obtain target-version source for third-party components outside this repository before relying on their behavior.

### Three context-supply paths

| Delivery path | Candidate behavior | Integration boundary |
|---|---|---|
| Native transparent supplementation | This scenario contributes `allow/nop`; other restrictions still apply | Planned retrieval and injection at `session.start`, `input.submit`, `compact.before`, `model.before_request` or supported pre-tool points. Native admission, injection and adoption remain pending |
| Blocking suggestion | `block` holds the current candidate and may explain an alternative | Any suggested retry is a **new candidate** through all original authorization and pre-execution checks; never count it as transparent supply |
| Post-tool result rewrite | The tool has already run | Only an explicitly admitted result slot can change. It neither undoes execution nor provides generic prompt injection; actual Adapter support must be verified |

For result rewriting, verify projection availability, text-slot and ordering constraints, source labels, original-result retention and native adoption. If compression and supplementation share a final replacement slot, define one combined step or enable only one; do not assume separate replacement steps compose. Retrieved text stays evidence and cannot grant permissions or override task constraints.

### Action safeguards

- **Snapshot:** `allow+[snapshot,audit]` stays held until the consumer authorizes the snapshot and confirms its matching effect, then freshly checks binding, candidate/resource state, evidence validity and the original deadline. Complete required audit as well. A snapshot taken while awaiting approval still needs revalidation after the reply.
- **Rollback:** coordinate workspace quiescence and evacuate every workspace cwd, then preview, obtain human confirmation and freshly revalidate before execution. Freeze and session/turn end satisfy neither quiescence nor cwd evacuation. If coordination is unavailable, offer a recovery suggestion/preview only; the operator must clear occupancy and reconfirm.
- **Cleanup:** delete only authorized checkpoints no longer needed for recovery. Pinned-state handling and receipt retention are separate lifecycles; checkpoint deletion cannot be assumed to release either. Until reclamation is verified, cap new snapshots and report capacity pressure.
- **Subject actions:** freeze and quarantine remain independent. Attempt authorized freeze before quarantine, but freeze failure/unknown must not prevent independently authorized quarantine that meets its own preconditions. A waiting, denied or failed kill absorbs neither action and removes no existing protection. Only confirmed termination of the same stable target and full control scope may make unexecuted freeze/quarantine redundant; human approval or dispatch alone is insufficient.

## Input, errors and verification

Both judgment commands read one UTF-8 JSON request from a file or stdin, bounded to 262144 bytes. Fields and enums are closed and bindings must match. Both require `semantic-arbiter/v1alpha1`. `check` requires mode `deterministic_check` and recorded `check_at_ms`. `evaluate` requires mode `model_evaluation`; its configuration must be a named file, and it rejects duplicate keys throughout its request, configuration and upstream replies. Input preparation has a time cap even when an input pipe remains open. `--help` shows usage; the [protocol reference](../../../../src/semantic-arbiter/docs/design/protocol.md) defines fields, digests, directives and receipts.

Evidence has a hard limit of 64 items; `budget.max_evidence_items` and `budget.max_evidence_bytes` can impose tighter bounds. Exceeded caller budgets cannot silently truncate evidence and permit execution. Evidence must satisfy `captured_at_ms <= evaluation_time < valid_until_ms` in the request's clock domain, including after inference.

| Result | Exit code | Output |
|---|---|---|
| Complete valid judgment, including model `unknown` with candidate `nop` or a valid `block` | 0 | Response JSON on stdout; inspect Outcome and judgment errors |
| Malformed/missing request or configuration, invalid binding, input timeout or I/O failure | 2 | Sanitized error JSON on stderr; input failures produce no stdout |
| Deadline, adopted evidence or dependent local context expires during allowance output | 2 | `deadline_exceeded` on stderr; discard all stdout, including partial responses |

`EnvironmentBackend` reads credentials only when inference is required. Missing/unusable credentials or remote authentication rejection become judgment errors and valid `unknown`/`nop` responses with exit `0`, unless a hard blocker applies. Purely local evaluation needs no credentials. `model_called` means backend invocation, not necessarily an HTTP request. Exit zero is completion, not permission. Output remains bounded by the original deadline, configured cap and adopted evidence/observation validity. Accept only complete zero-exit responses, inspect candidate control and revalidate before use.

Errors expose stable codes without paths, keys, source text or raw upstream bodies. Correct input/configuration failures before invoking again. For inference failures, retain the original record and reassess the candidate, evidence and deadline before another evaluation. For an unknown execution receipt, query the original receipt or verify the effect manually; do not automatically repeat a directive. Receipt correlation alone cannot prove authorization or execution.

For local development checks:

```bash
make check
```

This runs formatting checks, clippy, tests and strict rustdoc. Fixture probabilities verify contracts, not real model efficacy. Validate native Qoder/model integration separately in an authorized, isolated environment.

### AW shadow observation

Use the separate [semantic Provider](../../../../providers/semantic-arbiter/README.md) to compare complete native calls, with precise mappings or an opaque fallback, against the same session's captured prompts. It returns no execution effects. There is no installer/RPM integration for this Provider; follow its source-build instructions. Configure `aw-provider-semantic-arbiter` as the Provider command, not this core CLI. Review the paired synthetic [JSON](../../../../providers/semantic-arbiter/examples/config.json) and [AW YAML](../../../../providers/semantic-arbiter/examples/aw.yaml) templates and replace their paths before use.

Private Provider config accepts only declared fields; it contains no task text. This changes neither `aw/v1alpha1` nor `aw-provider/v1alpha1`, nor the core judgment protocol. Enable `capture_task_scope` on `input.submit` and `evaluate_task_scope` on `tool.before` (observe, `decision: true`, `mode: shadow`): each `input.submit` appends one turn's prompt to its session, and each `tool.before` is judged against all captured turns of that session. A prompt is unauthenticated user input and grants no permission; resource limits come only from `task.allowed_resource_ids` and `task.preserve_resource_ids`. Keep `model.config_revision: aw_request`; invocation substitutes a domain-separated hash of the full AW request configuration revision.

The Provider hashes `[adapter, binding_id, instance_id, native_session_id]` into the scoped session identity. Native sessions must be nonblank, at most 512 bytes and without control characters; Provider cwd and `event.native.cwd` must match the workspace. Unknown/unmapped tools or inputs and shell `no_mutation` results with valid object arguments retain the complete call as an opaque `command:unresolved` candidate. Invalid identity/context/task and invalid/oversized candidates remain uncovered. Native arguments are bounded to 64 KiB and the evaluation to 256 KiB; oversized input is retained intact in `input`, never truncated for inference.

`scenarios` defaults to `[task_scope]`. Local context is collected automatically; preservation/snapshot also collect Git rebuild evidence or explicit gaps. Missing knowledge may reach semantic fallback, while resource and evidence-integrity hard gates remain. The first captured prompt stores a bounded recursive metadata inventory; candidate observations use stable bounded reads. Exclusions, limits and unavailable Linux confinement remain gaps, not absence. See the [Provider collection contract](../../../../providers/semantic-arbiter/README.md#local-observations-and-judgment) for exact bounds. These stat facts do not attest security ownership; test/config/CI/dependency and side-effect signals are hints, not proof of malice, authorization or shared production use. Recovery remains advisory, with no business cleanup.

Use a trusted controlled workspace and reviewed Git configuration. Although the observation reader executes no candidate commands, the existing Git verifier still permits clean-filter execution and does not authenticate configuration/filter side effects; it does not make the whole Provider safe for untrusted workspaces.

After configuring the supported Agent and authorized backend, start AW from the exact configured workspace:

```bash
aw run --config /absolute/path/to/aw.yaml --agent target
```

**Authorize retention and outbound payloads first.** The private store contains full AW requests, every captured prompt in plaintext, native arguments and model observations; AW `metadata_only` audit does not disable it. Precreate the runtime-owned `0700` root without symlink components, preferably outside the Agent-writable workspace. Each session gets a private directory holding `task.json` (its captured turns) and `calls.jsonl` (one line per call stage); calls without an identifiable session go to `unbound.jsonl`. All files are `0600`.

Quotas are `max_sessions`, `max_session_bytes` and `max_storage_bytes`, counted in actual bytes on disk. Root quota `flock` contention waits only within the remaining original ingress budget, then skips inference on expiration; the lock is released before inference. Input waiting, parsing, projection, lock waiting, persistence and inference share the original minimum of AW `budget_ms` and model timeout: there is no separate renewed deadline or network retry. Stop users of the root before manually reviewing, archiving or deleting owned records.

Keep `effects: [observe]` and `on_error: report`. Valid invocations return `status: ok`, the original request digest and `effects: []`, even for advisory `block` or semantic failure. When the Host requests a decision, the reply also carries an advisory `allow`/`ask`/`block`/`abstain` recommendation and its obligations; an abstention requests none, and under `mode: shadow` AW records these without enforcing them. Inspect each call's `result` line for `evaluated`, `uncovered` or `skipped`, and the retained evaluation for actual judgments. AW and consumer integrations remain responsible for executing authorized recommendations; this Provider does not execute snapshots, block tools, roll back or clean up. No record or unsupported/unconfirmed result grants permission, performs protection or proves execution; native security and permissions remain authoritative.
