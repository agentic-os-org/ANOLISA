# AgentSight integration through the AW run lifecycle

[中文版](../../../zh/integrations/agentsight/agentsight-integration.md)

## Implementation and future design

The implemented read-only source connector, automatic observation and explicit context freezing are described in the [connector](agentsight-readonly.md) and [evidence contract](../../runtime/evidence-context.md).
This document covers ownership and future design. Full capability admission, `required` mode, public incremental audit, AgentSight audit consumption and semantic evaluation remain proposed.
See the [developer reference](../../../../../../../docs/developer-guide/en/aw/agentsight.md) for current configuration and integration.

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
admitted transport with explicit peer authentication.

## Proposed public audit and delivery

The proposed public `aw.audit/v1` is separate from private Journal and
service wire formats. Its envelope includes a stable event ID and kind,
authenticated producer scope/generation, AW/native references, causal reference,
source chain/sequence/digest, timestamps, applicable revisions, evidence/coverage
references and a kind-specific outcome. Unknown facts are null or explicit gaps.

The proposed export starts with existing facts, including binding preparation, launch attempt/outcome, observed
native callbacks, Provider invocation, reported native results, context freezing,
observation/coverage changes and event/run termination. Invocation completion does
not prove native adoption. An exit report does not prove complete observation.
The exporter must preserve these distinctions. Semantic events require the evaluation capability described below.

Critical local acknowledgement remains durable. Export and AgentSight consumption
are asynchronous, with stable replay IDs and bounded per-chain cursors. Consumers
revisit unfinished chains and report missing history, corruption and lag. Same
ID/content is a duplicate; same ID/different content is an integrity conflict.
Remote recovery never replays an AW action. AgentSight writes its own derived
store and does not parse or write private AW Journal files.

## Proposed capability admission and packaging

`required` mode needs evidence for each requested source capability before launch. Admission must distinguish query compatibility, trajectory collection and eBPF coverage; unsupported or unready required capabilities reject startup. Optional-source failures retain the existing bounded degradation behavior in the [connector](agentsight-readonly.md).

All-in-one packaging must select artifacts and profiles by capability, permit an AW-only installation and distinguish installed, enabled and ready. Deployment owns privileged probes and worker shutdown. Hooks must not install packages, change file capabilities or start root workers; ending one AW run must preserve shared services.

## Proposed semantic evaluation contract

The proposed capability is `semantic.evaluate/v1`, using the implemented [evidence context](../../runtime/evidence-context.md) as its input foundation. Model evaluation and policy routing require a separate implementation.

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

Provider v1alpha1 cannot carry these answers. Semantic evaluation needs an explicitly
versioned typed-result protocol plus service/configuration admission. It must not
hide answers in `observe` reason text, change old response shapes in place or
claim that a Provider handshake creates Core receipts. New capability schemas
belong to the AW contract registry; native Hook and Core adapters must each be
admitted against their actual guarantees.

## Future semantic routing and audit

The proposed sequence is: deterministic checks, select questions, acquire/freeze
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

Semantic configuration has independent `off`/`shadow`/`enforce` modes.
`off` disables the whole extension, not existing AW checks. `shadow` performs only
authorized evaluations and records proposed routes without invoking their action
Providers. `enforce` still requires per-policy capabilities and native acceptance.
No model weights, remote service or outbound credential is bundled implicitly.

Journal metadata retains judgment values, model/question/threshold revisions,
context digest, evidence references, selected route, action result and adoption
state. It does not retain complete prompts, tool content or private diagnostics
by default. Reproducing an evaluation requires a separately authorized immutable
redacted snapshot; hashes alone cannot reconstruct its input.

## Acceptance and rollback for the proposed extensions

Capability admission tests must cover absent collectors, incompatible sources, missing privileges and unavailable required capabilities. Audit export tests must cover replay, conflicting IDs, missing history and consumer lag. Semantic tests must cover binding mismatches, invalid answers, stale revisions, deadlines and unavailable required evaluations; retain existing Hook and observation regressions.

Each extension needs an independently selectable disable path. Before a service protocol rollback, drain owned runs and restart matching client/daemon binaries. Preserve Journal history and external AgentSight data. Packaging, orphan-owner recovery, full coverage policy and public incremental export remain future work.
