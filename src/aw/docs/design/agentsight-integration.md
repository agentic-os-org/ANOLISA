# AgentSight integration through the AW run lifecycle

[中文版](agentsight-integration_zh.md)

## Status and delivery

AW owns native Agent entry and local execution audit. AgentSight supplies optional
observations and later consumes a derived audit projection. Users configure the
source once and launch through `aw run`; no separate trajectory command or manual
session lookup is part of the normal workflow.

This design supersedes the standalone-query entrypoint. #7626 implements only the
read-only connector. #7655 depends on it and implements #7622, including the minimum
run lifecycle. Proposed contracts below are distinguished from the executable
schemas in that dependent PR. Semantic evaluation remains phase two.

| Stage | Owner | Depends on | Delivery |
| --- | --- | --- | --- |
| Bounded source connector | #7621 / #7626 | Existing AgentSight GET APIs | Library only; no `aw sight` |
| Run lifecycle, trusted association, evidence and context | #7622 / #7655 | #7626 | Startup, observation-only callbacks, automatic association, optional degradation, bounded exit cleanup |
| Full capability policy | #7623 | #7622 | Per-capability readiness, required admission, loss/recovery and coverage intervals |
| Public incremental audit | #7624 | #7622, #7623 | Stable export schema, enumeration, cursors and replay |
| AgentSight audit consumption | #7625 | #7624 | Authorized idempotent derived persistence |

#7626 is merged. #7655 builds on that mainline connector; its review diff contains
only the lifecycle/evidence stage. Neither stage changes semantic routing or independently redesigns the
PostTool scheduling and completion behavior merged in #7411.

## Component ownership

| Component | Responsibility |
| --- | --- |
| `aw run` and native Adapter | Prepare native integration, bind one AW instance, own the foreground Agent and temporary registrations, report actual exit and release the instance |
| `aw-service` | Retain configuration, corroborate native identities, manage bounded per-instance observation work, project evidence and record local metadata |
| AW Host and Core | Preserve admitted actions, original deadlines, native effect contracts and critical local audit failure semantics |
| AgentSight connector | Bounded GET requests with explicit transport trust, compatibility checks and explicit errors; no probe or database ownership |
| AgentSight runtime | Own collectors, query service and stores; report capabilities according to evidence |
| Deployment supervisor | Own optional all-in-one workers and their privileges, startup profiles and shutdown |

The existing same-user private socket is AW's trust boundary. Corroborated
callback fields do not isolate hostile callers sharing that Unix identity.

The deployment direction is local-first with an explicitly configured source.
AW owns Agent lifecycle and local audit; a separately managed AgentSight service
supplies optional observation. Literal-loopback HTTP remains supported without
credentials and reports `loopback_unauthenticated`. It neither authenticates the
listener nor makes its data suitable as sole authorization or execution proof.
Remote connections, including cross-container IPs, require HTTPS and private
credentials; loopback refers to AW's own network namespace. HTTPS reports
`bearer_supplied`, without claiming the source enforced it behind a proxy.
Both transports use the same [API contract checks](agentsight-readonly.md).
Future enforcement requiring authenticated local peers needs a separately
admitted transport; phase one does not infer that guarantee from a local port.

## Minimum lifecycle in #7622

1. Preparation validates configuration and Adapter support. A configured source in
   `auto` mode enables observation; `off` disables source I/O and additional native
   registrations. Unsupported `required` configuration must be rejected until
   #7623 implements its capability admission.
2. Binding creates an instance-scoped observation owner and performs a bounded
   source compatibility probe. Query readiness cannot establish collection or
   eBPF readiness. Optional source failure is recorded and does not prevent launch.
3. Register observation callbacks even when no action Provider is configured.
   Reuse existing callbacks when actions are present. Pure observation returns the
   native no-op response and cannot block, replace results or replay tools.
4. Corroborate session/call identity from admitted native callback bytes. The
   service automatically schedules bounded source reads. Hook acknowledgement
   waits for required local metadata, never for the external HTTP read.
5. Keep observation work scoped to the instance rather than an open tool event.
   Coalesce or reject excess work with an explicit gap; cap queued items, source
   bytes, projected bytes, retained sessions and query duration. Do not create a
   worker or HTTP request for every duplicate callback.
6. Normal exit and handled signals release only this instance, stop accepting
   observations, finish or discard bounded in-flight reads and remove owned
   native files. Record the reported exit separately from AW cleanup failure.
   Failed startup follows the same release path. An abruptly killed launcher may
   leave an incomplete run; it must never be labeled a successful completion.

The minimum implementation covers the native tool seams supported by each
Adapter. It does not claim every model turn, native Stop event or subagent is
observable. An empty run has a launch lifecycle but may have no associated native
session. A Stop/continue within a live Agent does not terminate the AW instance.
Source refresh uses bounded snapshots, not an invented streaming subscription.

## Identity and evidence

An AW instance, a native session and a source trajectory have distinct identities.
One instance can encounter multiple sessions. A later instance may resume a native
session; earlier source history remains historical evidence, not executions of
the new instance. Active cross-instance ambiguity must stay visible. Association
is corroborated against the admitted Adapter, not arbitrary source text or a PID.
Process correlation additionally needs reviewed host, boot and process-start
identity, which the first connector cannot supply.

`aw.evidence/v1` separates these field groups, with executable details in #7655:

| Group | Contract |
| --- | --- |
| Identity | Version, stable evidence ID, AW generation/configuration/binding/instance and event references |
| Source | Component, source scope, nullable source instance, record reference and observed revision |
| Association | Verified/unverified/unassociated, native session/call and corroboration basis |
| Times and coverage | Source time or unknown, receipt time, cutoff, explicit gaps and truncation |
| Content and derivation | Bounded content reference, encoding/digest, trust category and transformation version |
| Execution | Only corroborated native execution facts can carry an execution reference |

Model echoes and copied tool results do not add executions. Missing records,
unavailable storage, malformed results and unverified identities remain distinct.
Canonical metadata carries encoded content strings and digests for arbitrary
native JSON; it does not insert floating-point or arbitrary-key source objects
directly into the canonical domain.

## Bounded context

`aw.evidence-context/v1` binds the original candidate, constraints, selected
evidence and gaps to a subject and deterministic revision. Freezing preserves the
original event deadline and candidate; #7411 completion views cannot change them.
Once frozen, later source data cannot rewrite the snapshot or a decision.

Automatic observation and event context freezing have different lifetimes.
Observation may finish after a tool callback closes, producing separate metadata.
It cannot reopen that event or extend its execution budget. Phase one does not
freeze every Hook merely to exercise an API. The explicit service operation
remains available for embedding clients and later semantic consumers.

Selected source content stays in bounded memory or an explicit caller response.
Local Journal records contain identities, revisions, gaps and outcomes, without
credentials, environment snapshots or raw tool/history content. No automatic
redaction is claimed for content returned to an authorized caller.

## Audit contract and delivery

The proposed public `aw.audit/v1` in #7624 is separate from private Journal and
service wire formats. Its envelope includes a stable event ID and kind,
authenticated producer scope/generation, AW/native references, causal reference,
source chain/sequence/digest, timestamps, applicable revisions, evidence/coverage
references and a kind-specific outcome. Unknown facts are null or explicit gaps.

Phase-one facts include binding preparation, launch attempt/outcome, observed
native callbacks, Provider invocation, reported native results, context freezing,
observation/coverage changes and event/run termination. Invocation completion does
not prove native adoption. An exit report does not prove complete observation.
Only implemented facts enter the executable export; semantic events wait for
phase two.

Critical local acknowledgement remains durable. Export and AgentSight consumption
are asynchronous, with stable replay IDs and bounded per-chain cursors. Consumers
revisit unfinished chains and report missing history, corruption and lag. Same
ID/content is a duplicate; same ID/different content is an integrity conflict.
Remote recovery never replays an AW action. AgentSight writes its own derived
store and does not parse or write private AW Journal files.

## Degradation and all-in-one

| Condition | Minimum behavior |
| --- | --- |
| No source configuration or mode `off` | Existing AW actions and local audit continue; no source reads |
| Optional source unavailable, incompatible or unauthorized | Continue AW; expose a sanitized reason and unknown/unavailable evidence |
| Query API online, collector unknown | Query compatibility may be ready; collection and coverage stay unknown |
| No kernel/BTF/privileges for eBPF | Keep AW usable; report missing eBPF coverage; use supported user-space trajectories if separately available |
| Source content missing, delayed or over bounds | Record unknown/gap/truncation without turning it into proof of inactivity |
| Required capability requested | Reject until implemented; #7623 admits only explicitly proven ready capabilities |

AgentSight `trace --no-ebpf` can collect supported user-space trajectories, while
`serve` exposes queries. A live `serve` alone does not prove collection is running.
All-in-one packaging must select artifacts and profiles by capability, permit the
AW-only baseline and distinguish installed, enabled and ready. Deployment owns
privileged probes; `aw run` never installs packages, changes file capabilities or
starts root workers from a Hook. Ending one AW run cannot stop a shared service.

## Semantic evaluation contract reserved for phase 2

The proposed capability is `semantic.evaluate/v1`. Phase 1 reserves its input
foundation only. It does not add a Jev-like backend, invoke a model or interpret
probabilities as permission.

| Direction | Fields and validation |
| --- | --- |
| Request identity | `schema_version`, `request_id`, `aw_event_ref`, verified `binding`, `stage`, `input_digest` |
| Subject/context | `subject_ref`, `candidate_revision`, `context_revision`, authorized content projection, `evidence`, `cutoff`, remaining budget |
| Questions | Versioned stable ASCII IDs, `boolean`/`choice`/`score`, prompt, declared options or integer grades, requiredness and material requirements |
| Response binding | Echo request, input, candidate and context bindings; Core verifies all against the current attempt |
| Response status | `ok`, `abstain` or `error`; actual `model_version` is required as a field and nullable when unknown |
| Answers | Only declared question IDs, no duplicates, matching answer types, valid evidence references and bounded values; required answers must be present on `ok` |

Public probabilities and confidence use integer millionths: `probability_ppm`
and `confidence_ppm` in `0..1000000`. Choice/score distributions sum to `1000000`;
option/grade IDs must match the question definition. The backend adapter validates
finite raw outputs and applies a versioned deterministic quantization rule.
Threshold calibration includes that rule. Weighted scores are calculated in code
from declared integer grades/distributions, not accepted as a separate model fact.
Boolean probability and classification confidence have different meanings.

For `ok`, `answers` is required: Boolean supplies a proposition probability;
Choice supplies a declared `selected` option, option distribution and confidence;
Score supplies the declared grade distribution and confidence. Optional answers
may be omitted. For `abstain`/`error`, omit `answers` and return a non-sensitive
`reason_code` and affected question IDs; a partial answer cannot authorize the
remaining policy. Selected options and evidence references must exist in the
request, and unknown public fields are rejected by the versioned schema.

Insufficient evidence produces `abstain`; backend failure or invalid output
produces `error`. An unusable required answer makes the policy evaluation
unusable. Timeout, stale/wrong bindings, unknown mandatory model revision and
confidence below the configured minimum also take `on_unusable`. Existing
Provider execution failures remain distinct from valid policy denials.

Provider v1alpha1 cannot carry these answers. Phase 2 needs an explicitly
versioned typed-result protocol plus service/configuration admission. It must not
hide answers in `observe` reason text, change old response shapes in place or
claim that a Provider handshake creates Core receipts. New capability schemas
belong to the AW contract registry; native Hook and Core adapters must each be
admitted against their actual guarantees.

## Future semantic routing and audit

The phase-2 sequence is: deterministic checks, select questions, acquire/freeze
context, bounded evaluation, validate answers, select a configured route, admit
the resulting action plan, invoke registered actions, then return a supported
native effect. The original event deadline covers all stages.

Routes refer to registered actions and controlled inputs. Model-generated
commands, paths or new Provider identities are not executable routes. A later
route must not mutate an executing immutable Core plan; evaluation and action
plans need distinct, once-only stage identities under the same parent event.
Re-evaluation cannot reset a claim, renew a deadline or repeat a prior action.
Changed resources/candidates require fresh validation before dispatch.

`no_additional_action` is not permission. A required unavailable evaluation denies
the governed dispatch; an optional advisory evaluation records a gap. Protection
actions must succeed before dependent execution, and can be enabled only after
their own native/OS guarantees are accepted. Native approval, final guards and
Stop/continue control are not implied by a schema name or this design.

Semantic configuration has independent `off`/`shadow`/`enforce` modes in phase 2.
`off` disables the whole extension, not existing AW checks. `shadow` performs only
authorized evaluations and records proposed routes without invoking their action
Providers. `enforce` still requires per-policy capabilities and native acceptance.
No model weights, remote service or outbound credential is bundled implicitly.

Journal metadata retains judgment values, model/question/threshold revisions,
context digest, evidence references, selected route, action result and adoption
state. It does not retain complete prompts, tool content or private diagnostics
by default. Reproducing an evaluation requires a separately authorized immutable
redacted snapshot; hashes alone cannot reconstruct its input.

## Acceptance and rollback

The run closure must work without action Providers or eBPF. Tests cover startup
failure, zero callbacks, verified and forged IDs, resumed and conflicting sessions,
multiple instances, bounded slow sources, duplicate callbacks, queue overflow,
normal/signal exit and owned resource cleanup. Existing PostTool tests must pass.
HTTP/native fixtures do not certify real AgentSight probe coverage or live models.

Disable optional observation, exit owned runs and restart matching client/daemon
binaries to roll back service protocol changes. Preserve Journal history and
external AgentSight data. Packaging, orphan-owner recovery, full coverage policy
and public incremental export must not be presented as already delivered by the
connector-only PR.
