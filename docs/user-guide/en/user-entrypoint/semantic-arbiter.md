# semantic-arbiter

[中文版](../../zh/user-entrypoint/semantic-arbiter.md)

Evaluate preservation and task-scope questions with a configured decision model, or deterministically check frozen reference inputs at a recorded clock. `semantic-arbiter` returns typed recommendations without starting an Agent, replaying Agent actions or modifying a workspace. Consumers remain responsible for authorization and execution.

## Target design and availability

The complete target design covers six business scenarios: protect results, enforce task scope, supply context, diagnose problems, respond to security events and govern recovery resources. `semantic-arbiter` never executes actions, including in the target architecture. All Executors permanently belong to consumer integrations.

| Status | What it means in this guide |
|---|---|
| **Implemented core** | Fixed preservation/task-scope judgments, frozen inline evidence, deterministic `check` and Noul model evaluation through Bailian/TypeSafe |
| **Contract types/validation only** | Full `nop < allow < ask < block` CandidateControl vocabulary, wider Outcome types, Directive projection and receipt validation exist; current scenarios do not produce `ask` or wider actions |
| **Pending consumer integration** | Task/history collection and delivery, admission, native blocking, every Executor, persistent audit and effect verification still require integration; live EvidenceSource collection and general composition also remain pending |
| **External capability requires verification** | AgentSight, AW, ws-ckpt and native Adapter interfaces must be checked against target-version source and deployment behavior; historical capability inventories establish no support |

| Business scenario | Purpose | Proposed output | Availability |
|---|---|---|---|
| Preservation | Keep required versions; protect unique results before modification | `allow` with `audit`, add `snapshot` for unique results; `block` when preservation is required or proof is unusable | Implemented core: `preservation`; execution pending |
| Task scope | Check task intent and allowed resources; eventually include command structure and network destinations | `allow/block` with `audit`; a future deterministic approval rule may produce `ask` after integration | Implemented core: `task_scope`, currently resource `replace/delete` only |
| Context supplementation | Find missing skills, memories or other reviewed material | `retrieve_skill/retrieve_memory`, `inject_context`, `audit`; candidate handling depends on delivery path | Pending scenario and integration |
| Diagnosis | Assess failures, resource pressure and task progress; request assistance | `diagnose`, `notify`, `audit`; a later candidate may need `ask` | Pending scenario and integration |
| Security response | Assess event credibility and contain the affected subject | `block` before execution; `freeze/quarantine/kill` with `audit`, optionally `notify` | Pending scenario and integration |
| Cleanup and governance | Retain audit and recovery obligations while reclaiming eligible checkpoints | `nop` with `audit/cleanup`; recovery may suggest `rollback` | Pending scenario and integration |

Only `preservation` and `task_scope` are callable scenario names. Other rows describe planned behavior, not accepted request values.

### From evidence to a decision

The target flow is **EvidenceSource → JudgmentUnit/Scenario → validated answers → Outcome → consumer admission → Executor → Receipt**.

| Stage | Responsibility and boundary |
|---|---|
| Consumer and EvidenceSource | Authenticate task/candidate origins, bind identities and versions, and freeze bounded evidence. Target sources declare provenance, freshness, size and collection deadlines; today the caller supplies inline evidence |
| JudgmentUnit and Scenario | A unit selects required evidence, a versioned question and its threshold; a scenario groups units for a business purpose. Today there are two fixed units and fixed composition |
| Validated answers and Outcome | Validate answer shape, evidence references, coverage and expiry, then deterministically map answers to restrictions and obligations. Model support can only narrow deterministic constraints |
| Consumer admission and Executors | Check declared capability, configured permission, native Adapter support, identity, resource scope and remaining deadline; hold the candidate while required protections are unconfirmed |
| Receipt and audit | Retain the immutable directive, verify its effect and record each result. Sending a request or receiving a successful transport response proves no effect |

Consumer integration must capture task text and retention requirements from an authenticated user/operator entry, retain the current task and version independently of bounded session history, and mark omitted or truncated history. Freeze the candidate, workspace, task and configuration revisions together. Collection, inference, protection and final revalidation share the original event deadline; auxiliary collection never renews it. Changed task, candidate or policy revisions require a fresh judgment.

AgentSight is intended to supply auxiliary history, observations, trends and forensic reports. Missing reports cannot mean permission, and projecting `source_reported` through a consumer cannot promote it to user authorization or `local_verifier`. Local reconstruction requires comparing bytes rebuilt from retained generators and old inputs; the consumer supplies and authenticates that result. The core neither runs generators nor checks disk contents. Workspace identity and resource scope require independent verification; cwd alone proves neither containment nor permission.

## Build from source

Installer and RPM distribution are unavailable. Use a Linux checkout with Rust/Cargo, rustfmt, clippy and Make. From the repository root:

```bash
cd src/semantic-arbiter
make build
./target/debug/semantic-arbiter describe
```

`describe` lists `deterministic_check` and `model_evaluation`, the `bailian` and `typesafe` backends, the `preservation` and `task_scope` scenarios, and the message size limit. Current `check` output uses candidate `allow/block`; current `evaluate` output uses `nop/allow/block`. Both produce only advisory `snapshot/audit` obligations. No daemon is required.

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

`evaluate` sends task text, the candidate and selected provenance-labeled evidence to the configured service. Authorize the endpoint, credential use and outbound data before running it. Use the supplied synthetic samples first. Remote availability and semantic accuracy require separate validation.

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
| `thresholds.preservation_ppm`, `thresholds.task_scope_ppm` | Independent support thresholds, `500001..=1000000`; both default to `900000` |
| `transport` | Default `https`; `loopback_test` permits HTTP only on literal loopback IPs and labels responses as testing |

The Bailian example explicitly uses `800000` for both thresholds; the TypeSafe example uses `900000`. These values have no established accuracy calibration. The response's `config_digest` binds effective configuration and versioned question semantics; `config_revision` remains the caller-maintained policy label.

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
| `preservation` | Does task intent require keeping the resource unchanged? Yes support meeting the threshold yields `preserve`; no support meeting it retains the local rebuild result: `allow` for identical bytes or `needs_snapshot` for unique results |
| `task_scope` | Does the candidate operation satisfy task intent? Yes support meeting the threshold yields `allow`; no support meeting it yields `outside_scope` |

`evaluate` reuses the deterministic hard gate. Structured retention, allowed-resource limits, missing task/proof and coverage gaps remain binding and hard blockers remain `block`. At most one batched backend call is made; no call is made when no unit is eligible. The registry and composition are fixed.

Inspect these response fields:

| Field | What to check |
|---|---|
| `answers` | Per-unit conclusion, reason, confidence and evidence references |
| `outcome.control.candidate` | Aggregate candidate disposition: current evaluation can produce `nop`, `allow` or `block` |
| `outcome.obligations` | Required advisory `audit`, plus `snapshot` only after a completed preservation judgment supports it |
| `judgments` | Question/unit versions, threshold, raw P(yes), normalized yes/no support, `model_used` and sanitized errors |
| `backend`, `model_called` | Constructed adapter identity and whether it was invoked |
| `input_digest`, `config_digest`, `binding` | Association with the retained request and configuration |
| `evaluated_at_ms`, `elapsed_ms`, `normalization_revision` | Final check time, elapsed preparation/evaluation time and probability conversion version |

Only Noul model answers are supported. Each probability represents P(yes) for one proposition. Yes/no support is conservatively normalized in integer millionths from exact decimal tokens; confidence has no established calibration to factual correctness. Upstream `model`, `usage`, `request_id` and `latency_ms` are accepted as ignored metadata. Unknown fields, duplicate keys, mismatched questions and malformed probabilities become model-response errors, yielding an `unknown` answer and candidate `nop` unless a deterministic hard blocker already applies.

Unit answers and candidate disposition have separate meanings. Deterministic hard blockers remain candidate `block`. For a model-eligible question, low bilateral confidence, timeout, or backend/model-response error yields an `unknown` answer and candidate `nop`, never `ask`; there is no fixture fallback. Preservation model abstention retains `[audit]` only and does not carry the deterministic baseline's snapshot. Evidence validity is rechecked after inference and composition.

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

Expected candidate/obligation combinations are, in order, `allow/[audit]`, `block/[audit]`, `allow/[snapshot,audit]`, `block/[audit]` and `allow/[audit]`. `examples/index.json` records expectations. Deterministic unknown fails closed. `check` uses `semantic-arbiter/v1alpha1` with mode `deterministic_check`; model evaluation remains `semantic-arbiter/v1alpha2` with mode `model_evaluation` and real-time timestamps. Pass check samples through the synthetic generator before model evaluation.

## Outcome design reference

Outcome separates candidate restrictions, persistent subject requests and ordered obligations, with `evidence_refs` identifying adopted evidence. The full CandidateControl interface applies to every operation and future scenario; the broader vocabulary below is **contract types/validation only** where noted. Current `check` output is limited to candidate `allow/block`; current `evaluate` output is limited to `nop/allow/block`. Both have empty `control.subject[]` and only `snapshot/audit` suggestions. All execution remains pending consumer integration.

### Candidate control

| Value, in increasing strictness | Meaning |
|---|---|
| `nop` | No judgment formed; audit the gap. Suitable for an unavailable optional enhancement, never a fallback for missing protection evidence |
| `allow` | Judgment completed without objection; grants no execution authority |
| `ask` | A deterministic decision that this candidate requires human approval; hold it while waiting and on timeout. Approval requires fresh context checks and proves neither authorization nor execution |
| `block` | Prevent this candidate through the consumer's native blocking interface; cannot undo an operation already executed |

Target composition takes the strictest result, `nop < allow < ask < block`; a later enhancement cannot cancel a block. Candidate control lasts for one candidate. `ask` is never inferred from ambiguity: its types, validation and Approval Directive projection are reserved now, but current scenarios do not produce it and runtime `ApprovalExecutor`/Adapter admission remains pending A4/A10. An `unknown` unit answer means insufficient semantic support. Deterministic `check` maps such protection uncertainty to `block`; an eligible model abstention in `evaluate` produces candidate `nop` unless a deterministic hard blocker already requires `block`. An unknown execution receipt is separate again: it means an effect was not confirmed.

### Subject control

| `action` | Effect sought | Lifecycle and authorization |
|---|---|---|
| `freeze` | Suspend execution while retaining state | Persistent until separately authorized resumption; does not vacate cwd |
| `quarantine` | Restrict access to network, sensitive paths or credentials | Independent of suspension; lifting restrictions requires new authorization |
| `kill` | Terminate the subject | Irreversible; requires stable target identity, scope checks and human confirmation |

Each `SubjectDirective` carries `id`, bound `instance_id`, `action` and nonempty consumer-resolved `scope_ids`. A bare PID is insufficient. Target composition unions independent protections across judgments without implicitly removing existing protection. Requests describe desired actions, not confirmed process state; each needs its own authorization and receipt.

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

Target composition unions obligations with explicit execution order and independent receipts. Protection always fails closed: unconfirmed required work holds dependent execution, even with `critical=false`. Enhancements normally fail open with visible failures; `critical=true` upgrades them to protection semantics. Every nonempty Outcome requires exactly one `audit`, including subject-only control with candidate `nop`; evidence references and enabled-scenario gaps also require audit.

### Directives and receipts

`Outcome::directives` projects `ask`, subject requests and obligations without execution, scheduling or merging. `allow/nop` have no action; `block` uses native consumer denial. Associate each retained immutable directive and receipt by **`request_id + binding + directive_id`**, never by action name alone.

| Receipt `state/reason` | Meaning |
|---|---|
| `applied/confirmed` | Target action independently verified as effective |
| `rejected/denied` | Target action confirmed not to have taken effect; no retry permission implied |
| `unknown/unconfirmed` | Effect cannot be determined; query the original receipt or verify manually before further action |

An unknown receipt concerns execution, unlike an unknown judgment answer, candidate `nop`, or candidate `ask`. Never automatically repeat a directive with an unknown effect or release a candidate depending on it. Current `decode_receipt` validates check-protocol correlation and state/reason consistency using `CheckRequest` and the retained Outcome; it supplies neither a model-evaluation receipt decoder nor an OS-effect checker. Neither operation produces execution receipts.

## Consumer integration design

**All Executors below are pending integration and remain outside semantic-arbiter.** Admission must intersect configured permission, declared outputs, Executor support and verified native Adapter capabilities, then check identity, scope and deadline. Unsupported protection keeps the candidate blocked. Consumers own scheduling, per-directive idempotency, quotas and durable audit.

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

Both judgment commands read one UTF-8 JSON request from a file or stdin, bounded to 262144 bytes. Fields and enums are closed and bindings must match. `check` requires `semantic-arbiter/v1alpha1`, mode `deterministic_check` and recorded `check_at_ms`. `evaluate` retains `semantic-arbiter/v1alpha2` and mode `model_evaluation`; its configuration must be a named file, and it rejects duplicate keys throughout its request, configuration and upstream replies. Input preparation has a time cap even when an input pipe remains open. `--help` shows usage; the [protocol reference](../../../../src/semantic-arbiter/docs/design/protocol.md) defines fields, digests, directives and receipts.

Evidence has a hard limit of 64 items; `budget.max_evidence_items` and `budget.max_evidence_bytes` can impose tighter bounds. Exceeded caller budgets cannot silently truncate evidence and permit execution. Evidence must satisfy `captured_at_ms <= evaluation_time < valid_until_ms` in the request's clock domain, including after inference.

| Result | Exit code | Output |
|---|---|---|
| Complete valid judgment, including model `unknown` with candidate `nop` or a valid `block` | 0 | Response JSON on stdout; inspect Outcome and judgment errors |
| Malformed/missing request or configuration, invalid binding, unusable/missing credentials, input timeout or I/O failure | 2 | Sanitized error JSON on stderr; input failures produce no stdout |
| Deadline or adopted evidence expires during allowance output | 2 | `deadline_exceeded` on stderr; discard all stdout, including partial responses |

A remote authentication rejection during inference is recorded as a judgment error with an `unknown` answer and candidate `nop`, unless a deterministic hard blocker already requires `block`; it exits `0`. Missing credentials during backend construction exit `2`. Exit zero means a complete judgment, not permission. Allowance output uses nonblocking writes bounded by the original deadline, configured cap and adopted evidence validity. Accept only complete responses with exit zero, inspect candidate control, then revalidate before later use.

Errors expose stable codes without paths, keys, source text or raw upstream bodies. Correct input/configuration failures before invoking again. For inference failures, retain the original record and reassess the candidate, evidence and deadline before another evaluation. For an unknown execution receipt, query the original receipt or verify the effect manually; do not automatically repeat a directive. Receipt correlation alone cannot prove authorization or execution.

For local development checks:

```bash
make check
```

This runs formatting checks, clippy, tests and strict rustdoc. Local checks cannot establish remote availability or model accuracy. An AW adapter remains pending and belongs in the repository's `providers/` directory; this binary cannot be configured as an AW Provider.
