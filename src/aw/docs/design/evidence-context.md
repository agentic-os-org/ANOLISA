# Trusted evidence and bounded context

[中文版](evidence-context_zh.md)

## Scope and dependencies

This implements [#7622](https://github.com/agentic-os-org/ANOLISA/issues/7622)
on the read-only connector in [#7626](https://github.com/agentic-os-org/ANOLISA/pull/7626),
which implements #7621 and is now merged. This stage builds on main, preserving [#7411](https://github.com/agentic-os-org/ANOLISA/pull/7411) merged at
`d217dd6bbab927a9820202de5944adf3d9e4a5d7`.

The service offers an explicit `freeze_context` RPC for an active event. It binds
the original candidate, current AW constraints and selected history to one
immutable revision. `aw run` does not automatically issue this request. No model,
semantic evaluation, action routing or new Provider operation is introduced.
Coverage admission belongs to #7623, incremental public audit export to #7624,
and AgentSight consumption to #7625. Semantic evaluation remains phase two.

## Automatic run observation

`aw run` starts optional observation after binding and before spawning the Agent.
`start_observation` probes query compatibility within the configured timeout,
capped at one second. `report_run` records actual spawn/exit or launch failure;
PIDs remain launcher-reported data. `status.observations` exposes metadata only.

Native registration includes an internal `$aw.observe` callback when an event has
no admitted actions. `observe_hook_event` corroborates its original input without
opening a Host action event; it rejects attempts to use this path for configured
action steps. Existing action callbacks schedule observation once on event open.
Both paths return independently of external HTTP, preserving #7411 scheduling.

An instance owns one worker, a two-candidate queue and one bounded in-memory
projection. At most 256 callbacks enter observation per instance. Input limits,
queue overflow and worker/source failure produce visible gaps. Automatic reads
cap at one second and the original callback deadline; selected content follows
the configured item/byte limits. Their projections do not freeze or mutate an
active event. Explicit `freeze_context` retains its original separate semantics.

`release_instance` and `unbind` stop admission and drain bounded observation work
before dropping only that instance's source claims and buffers. Service shutdown
also drains workers. Startup-in-progress is included in cleanup, so interruption
cannot detach a late worker. The launcher waits at most five seconds for release
and reports cleanup errors. A killed launcher can leave incomplete state until
service shutdown; automatic orphan-owner recovery is not implemented.

`evidence.mode` accepts `auto` (default) and `off`. Optional source failure leaves
AW actions and audit active; off performs no source I/O or extra registration.
Required capability admission remains #7623 and unsupported values are rejected.
The probe checks only the auth-status response contract. Storage, collection and
eBPF readiness remain unknown; actual trajectory reads validate ATIF separately.
Neither transport requests `/health` or gates on a product version. Source product
version remains null throughout evidence projection and canonical digests.
See the [lifecycle design](agentsight-integration.md) for downstream audit and
semantic contracts.

## Foundation for phase-two semantic evaluation

The future evaluation stage can bind its input to AW's admitted subject, original
candidate and constraints, then use `aw.evidence/v1` references and one frozen
`aw.evidence-context/v1` revision. The snapshot records projection versions,
cutoff, bounds, provenance and gaps; it keeps native results distinct from source
history and model echoes. Repeated reads cannot replace the candidate or renew
its deadline. Metadata-only Journal receipts connect later decisions to the
exact input revision without persisting raw context.

This is input infrastructure, not a semantic service interface. `freeze_context`
currently requires an active action event; automatic observation does not create
one or invoke evaluation. Phase two must admit its evaluation stage, material
requirements, typed response contract and action routing separately. Required
missing/stale material must follow that policy's unusable-input behavior; optional
Sight history cannot become an implicit prerequisite for every decision.

The default user-space trajectory scan waits 30 seconds between rounds. A GET
reads the stored snapshot and does not trigger collection, so another immediate
GET cannot guarantee freshness. Source time remains unverified in this projection.
Hooks do not wait for the next scan; late records cannot rewrite a frozen context
or completed decision. AW's native callbacks remain the baseline for timely
input and local audit. See the [phase-two contract](agentsight-integration.md#semantic-evaluation-contract-reserved-for-phase-2).

## Owners and trust boundary

| Component | Responsibility |
| --- | --- |
| Trusted same-user launcher / Adapter | Supply admitted capabilities and original native callback bytes |
| `aw-service` | Issue generation, binding-instance and event identities; restrict the source; corroborate native IDs; freeze and retain one bounded in-memory context per active event |
| `aw-contracts` | Offline `aw.evidence/v1` and `aw.evidence-context/v1` schemas, canonical digests and consistency validation |
| AgentSight connector | Bounded GET requests using the common auth-status / ATIF-v1.7 contract and the selected transport authentication mode |
| Host / native Adapter | Existing admission, scheduling, replacement, cancellation and adoption behavior |
| Journal | Synchronously commit context revision, evidence IDs and gaps; never retain context contents |

The private same-user socket is the existing trust boundary. These checks do
not isolate hostile programs running under the same UID or authenticate a remote
Agent process. Loopback HTTP reads no credential and sends no Authorization
header (`loopback_unauthenticated`). HTTPS uses a private token file and normal
TLS verification; `bearer_supplied` means a token was sent, not proof of source-side
enforcement. Neither transport verifies AW identity. Source content remains analysis data, including when its session
association has been corroborated. It cannot change AW policy or authorize an action.

## Identity association

Generation and configuration revision come from the running service. Binding ID
comes from the prepared Host target, instance ID from service admission and
event ID from the event registry. The caller must provide the owning instance
and the `candidate_revision` returned in the event handle. An unrelated instance,
stale generation, stale candidate, closed event or expired event is rejected.

`open_hook_event` and observation-only `observe_hook_event` corroborate native identity. Evidence checks
compare the exact decoded callback with the retained native value, session and
call IDs, event seam, tool name, input and original result. The admitted Host's
Adapter selects the comparison. Plain `open_event` IDs remain unverified; absent
IDs remain unassociated. Existing Hook admission still requires nonempty native
session/call IDs; nullable evidence fields do not weaken it.

Qoder and QwenPaw preserve native call IDs. Hermes preserves the existing hash
of API request ID and tool-call ID, with its `hermes:` prefix. OpenClaw preserves
the hash of run ID and tool-call ID. The same raw model call ID in another native
run cannot match the original callback. This work does not use Core runtime-binding
session claims, so it does not rely on the unchecked omission case in
[#4372](https://github.com/agentic-os-org/ANOLISA/issues/4372); that issue remains separate.

Each configured target supplies its source origin; only HTTPS requires an absolute
private token-file path. HTTP may omit it and ignores a supplied path. The RPC
accepts neither an endpoint nor a source session selector. A source query derives
its session exclusively from the corroborated callback; its root session must
match exactly. Framework labels use this explicit source contract:

| AW Adapter | Accepted `agent.name` |
| --- | --- |
| `qoder` | `qoder`, `Qoder` |
| `hermes` | `hermes`, `Hermes` |
| `openclaw` | `openclaw`, `OpenClaw` |
| `qwenpaw` | `qwenpaw` |

Qoder's system collector reports the lowercase ID; local discovery reports the
display name. Hermes/OpenClaw display names match AgentSight's default configuration;
this does not certify trajectory collection for those frameworks. Other names,
including `QoderWork`, `QwenCode` and `AgentScope`, are not inferred as aliases.
Custom sources must satisfy both this label contract and the native session match.
Name matching never establishes a native binding or process identity.

For a given normalized origin, Adapter and native session, the first querying
AW instance owns the mapping until its events and observation worker drain. Another active instance gets
`source_session_conflict` and no history, even under another configured target.
The service retains at most 1024 active mappings without eviction. Release removes
only the drained owner's claims. A resumed instance can associate old history,
which never becomes new executions. Restart invalidates all old handles.
Operators remain responsible for selecting authorized source origins and scopes.

The API does not expose a verified AgentSight runtime-instance or host/boot/process
start identity. Source instance stays null and process association is unavailable.
The v1 schema deliberately has no process-association basis: neither a PID nor a
PID plus claimed metadata can produce a verified process assertion. Adding that
basis requires a reviewed source contract with host identity, boot identity and
process-start identity; PID/name equality alone is insufficient, including across
PID reuse. This limitation is reported as `process_identity_unavailable`.

## Executable evidence contract

[`evidence-v1.schema.json`](../../schemas/evidence-v1.schema.json) is the
authoritative shape. It rejects unknown fields; metadata uses the AW canonical
domain. Every record has:

| Field | Meaning |
| --- | --- |
| `schema_version`, `evidence_id` | `aw.evidence/v1`; digest of `[source, content_ref]` |
| `subject` | Service generation, exact configuration revision, target binding, instance, event and admitted Adapter |
| `source` | Component, scope, nullable runtime instance and product version, record reference and revision |
| `association` | `verified`, `unverified` or `unassociated`; `native_callback`, `native_session` or `none`; nullable native IDs |
| `times` | Nullable source occurrence string, local receive and snapshot cutoff time in Unix milliseconds |
| `kind`, `content_ref` | `native.callback`, `native.tool_result` or `trajectory.step`; SHA-256 reference to encoded content |
| `coverage` | Explicit `unknown` and bounded reason codes; no completeness assertion |
| `trust`, `derivation` | Trusted Adapter report, unverified native data or source-reported history; copied context stays a model echo |
| `execution_ref` | Only a verified native result can assert an execution identity; otherwise null |

Provenance constraints apply even when `execution_ref` is null. Both native kinds
require source `aw` and derivation `native_report`. Verified native records require
`trusted_adapter`, `native_callback` association and a tool call ID; unverified or
unassociated native records retain `unverified_native` and association basis `none`.
Trajectory steps require source `agentsight`, trust `source_reported`, derivation
`source_history` or `model_echo`, and no tool call or execution assertion. A verified
trajectory association uses `native_session`, never `native_callback`. These rules
check consistency; neither schema validation nor content digests authenticate a source.

AW source scope is the service generation. AgentSight scope is the canonical
digest of configured `[source_id, normalized_origin]`; its runtime instance is
unknown. History record references name the selected session and root step index.
Source revision hashes the complete decoded trajectory serialized as compact
JSON. This is a projection digest, not a claim about original wire bytes or the
source database revision. Occurrence strings are retained only up to 128 UTF-8
bytes; their clock/format/freshness is unverified. Receive time and cutoff do not
prove when an action happened, so historical content cannot establish a fresh
execution fact.

Execution identity hashes `[generation, binding_id, instance_id, session_id,
tool_call_id]`. A verified after callback contributes one identity; before
callbacks contribute none. ATIF steps never contribute executions, even when
their call ID or result text matches. Repeated callback opens reuse the original
event, and snapshots expose unique execution references. History records retain
their distinct source references instead of being merged by text or model IDs.
`is_copied_context: true` is labeled `model_echo`; unmarked history is still only
`source_history`, never independent proof of another execution or failure count.

## Snapshot selection and lifetime

[`evidence-context-v1.schema.json`](../../schemas/evidence-context-v1.schema.json)
defines the result. It includes the subject, original candidate revision, a
constraints content reference, selected evidence, content objects, source status,
gaps, limits, cutoff and the original monotonic deadline as a decimal string.

Candidate content contains the original event name and tool object. Constraints
contain the exact configured `execution` and `events`, including guards and error
actions, plus the revision of the complete configuration. Provider commands,
private settings, callback environment and credentials are not copied into the
context. Configuration identity pins those settings without exposing their values.
The projection version is `aw.evidence-projection/v1`; the redaction version is
`selected-fields-no-redaction/v1`. Selected tool/history content is not redacted.

Each content object has `encoding: json-utf8`, exact `text` and a digest of that
UTF-8 text. Arbitrary JSON numbers and Unicode keys live inside the string, never
in canonical metadata. `context_revision` is the canonical digest of the entire
result with only that revision field omitted. Changing the candidate, constraints,
selected content, source revision, limits, cutoff or projection changes the digest.
The offline validator checks the expected subject/candidate, content digests and
references, evidence IDs, bounds and execution identities. It validates consistency,
not source authenticity; consumers must obtain the snapshot from the trusted service.

Selection always retains the candidate and constraints intact. It selects the
newest root trajectory steps in source order within the item bound; the native
record consumes one item. Embedded subagents are excluded with a gap. If a selected
step would exceed the complete serialized context budget, it is omitted and
`content_byte_limit` is reported. A base candidate/constraints pair that cannot
fit fails visibly; it is never summarized or silently truncated.

One event can attempt freezing once. Concurrent requests report `context_busy`;
a completed request returns the exact same snapshot while the event is active.
A failed attempt cannot obtain a new budget. HTTP reads hold no event or Journal
lock. Their deadline is the minimum of the original Host deadline, request
deadline and configured query timeout. Shorter source timeout yields an explicit
gap when the event still has budget. Expiry or closure prevents publication.
In-flight blocking transport may finish at its admitted deadline; its late result
is discarded. Closing an event does not wait for that transport.

Late source changes cannot rewrite an existing context. After closure, full
context content is released and the old handle cannot read or recreate it.
For later analysis, a new explicitly admitted context/event is required; it does
not resume the old event or replay any tool. No background cache or content store
is introduced. Events with no pending native steps may finish before a caller
can request a context; evidence access does not extend their lifetime.

## Audit, compatibility and no eBPF

Before returning a frozen snapshot, the service synchronously appends a private
`phase: context_frozen` record containing candidate/context/source revisions,
evidence IDs, projection version and gaps. No candidate, history, secret or prompt
content is persisted. Audit failure remains fatal at the existing service
boundary. This is not the public `aw.audit/v1` export planned in #7624.

The local RPC becomes `aw-service/v1alpha3`: event handles gain
`candidate_revision` and `freeze_context` becomes available. Client, callback
binary and daemon must be upgraded together and old daemons restarted through
their owner. Provider stdio remains `aw-provider/v1alpha1`. Existing configuration
without `agents.<target>.evidence` remains valid. See the
[user reference](../../../../docs/user-guide/en/user-entrypoint/aw-agentsight.md)
for source configuration, limits and rollback.

#7411's final Qoder ordering uses trusted Host capabilities and is preserved.
Hermes final seams and OpenClaw replacement/completion views retain the original
event identity, deadline and candidate. A later completion view does not mutate
an already frozen context. This PR changes no Adapter scheduling or result adoption.

No eBPF probe, process discovery, privileged worker, package installation or
AgentSight lifecycle action is introduced. Without eBPF, available user-space
trajectories can still supplement the AW candidate. Missing service/store,
authentication/version failure and source limits produce explicit gaps; AW Hooks
and local audit retain their existing behavior. An empty valid trajectory is
`available` with unknown coverage; a 404 is `unknown`, not proof of no activity.
The minimum `auto`/`off` behavior above is implemented; full per-capability
`required` admission and all-in-one packaging remain separate work.

## Validation

HTTP/service fixtures cover scope isolation, forged/missing native identity,
PID reuse without process association, source mismatch/absence, repeated echoes,
late records, stale subjects/candidates, restart, item/byte/deadline limits,
concurrent freeze/close, audit poisoning and OpenClaw completion compatibility.
All four native identity formats have trust/scoped-call tests. The full AW gate
also exercises #7411's native ordering and replacement suite. These checks do not
certify a live AgentSight deployment, source-issued identity or eBPF coverage.
