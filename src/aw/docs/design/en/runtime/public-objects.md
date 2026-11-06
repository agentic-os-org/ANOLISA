# Public runtime and audit objects

[中文版](../../zh/runtime/public-objects.md)

`aw-contracts` defines a metadata contract for runtime producers and consumers
such as AgentSight. Objects use `apiVersion`, `kind`, `metadata` and `data`;
configuration retains its existing `spec` contract. The library validates objects and immutable references offline. `aw-service`
produces these facts at native Hook and launcher boundaries, independently of
optional AgentSight reads. Public audit transport and Sight ingestion consume
this retained metadata through separate integrations.

## Registered shapes and APIs

| Registry name | Object identity | Responsibility |
| --- | --- | --- |
| `object-meta-v1alpha1` | A value object, without its own kind | Shared metadata and `$defs` for references, subjects, association and coverage |
| `execution-event-v1alpha1` | `runtime.aw/v1alpha1`, `ExecutionEvent` | Metadata captured at an execution boundary |
| `audit-record-v1alpha1` | `audit.aw/v1alpha1`, `AuditRecord` | Metadata projected from one retained source-chain position |

The authoritative resources are in
[`aw-contracts/schemas`](../../../../crates/aw-contracts/schemas).
`Registry::object_schema` resolves exact version/kind pairs from those resources.
`Registry::validate` checks a named schema's shape;
`Registry::validate_object` additionally checks association, scope, source
identity and outcome invariants. Format validation includes actual calendar dates.
Untrusted JSON must first pass `canonical::parse`, which rejects duplicate keys,
unsafe integers, floating-point spellings and oversized/deep documents.

```rust
use aw_contracts::{canonical, objects, Error, Registry};
use serde_json::Value;

fn checked_reference(registry: &Registry, input: &[u8]) -> Result<Value, Error> {
    let object = canonical::parse(input)?;
    objects::reference(registry, &object)
}
```

`objects::reference` validates its target and returns a versioned reference with a
content digest. `objects::validate_reference` compares every reference field.
`objects::validate_audit_event` also checks an audit record's copied event name
and subject against the referenced event. A standalone valid audit record has
not established that relationship until the target is available and checked.

## Shared fields

All objects reject unknown fields. New fields use lower camel case. The shared
schema is a value vocabulary; it does not install a configurable object server.

| Value | Fields and meaning |
| --- | --- |
| Metadata | `uid`, `createdAt`, `producer`; facts are immutable, with no mutable status or arbitrary labels |
| Producer | `producerId`, `scopeId`, `generation`; retained source identity, scoped by the authenticated deployment |
| Reference | `apiVersion`, `kind`, `uid`, full `producer`, `digest`; the producer scope and generation travel with the reference |
| Digest | `algorithm: sha256`, `value`: 64 lowercase hex characters |
| Execution subject | `adapterId`, `bindingId`, `instanceId`, `configRevision`, nullable `sessionId`, `turnId`, `toolCallId` |
| Native references | Nullable `sessionId`, `turnId`, `toolCallId`, `modelCallId`; exact native values, separately from normalized identities |
| Association | `status`, `basis`, `verifiedFields`, `gaps`; verification applies only to the named normalized subject fields |
| Event times | Nullable `occurredAt`, required `receivedAt`; metadata `createdAt` is the retained object-creation time |
| Observation coverage | `status`, `scope`, `interval`, `observedAt`, `gaps`, `truncated`; a claim about an explicit set of events within one subject |

IDs in metadata, producer, binding and chain fields contain 1–128 printable ASCII
characters. Normalized session/turn/call and native identifiers permit 1–512
Unicode characters without control characters; never truncate identifiers.
Adapter IDs are 1–64 lowercase ASCII letters/digits/hyphens, starting with a
letter. A valid ID does not establish that an Adapter is registered or supports
a native capability; runtime admission retains that responsibility.

The producer retains `producerId` with its audit store; `scopeId` identifies the
authenticated deployment scope. A runtime assigns a fresh `generation` for each
writer lifetime and retains it on historical objects. An ExecutionEvent producer
allocates its opaque `uid` once when capturing the fact, ensures uniqueness
within that producer identity and persists it for replay. Readers never regenerate
it from arrival time or native call IDs. The runtime owns binding/instance IDs;
session, turn and tool IDs are interpreted within their binding/instance and
session context. Native ID equality alone does not identify a public object.

Times use the fixed UTC form `YYYY-MM-DDTHH:MM:SS.sssZ`. Missing native times and
applicable-but-unknown identities use `null`, not empty strings or invented IDs.
Configuration revisions hash the original configuration bytes. Input/result digests hash the UTF-8 JSON serialization of the selected normalized
input/result value at capture time. They do not preserve the original native JSON
spelling or prove that the body is retained. Object digests use the existing
integer-only AW canonical metadata encoding. Metadata never includes the raw
prompt, tool arguments/results, environment or native callback envelope.

`verified` requires a nonempty `verifiedFields` set and an explicit basis;
each named subject field must exist. Native IDs cannot appear without their
normalized counterpart. `unassociated` carries no request identity, has basis
`none`, an empty verified set and a gap. Other unverified associations also
carry gaps. A producer can verify a tool identity while leaving the turn native
ID unknown. The validator checks consistency; authentication and the native
proof belong to the producer.

Coverage names an `instance`, `turn` or `tool` level and one or more event kinds.
Turn/tool coverage requires the corresponding subject identity. `complete`
requires both interval endpoints, no gaps and no truncation. `unknown`, `partial`
and `unavailable` require gaps; known endpoints cannot form a reversed interval.
A complete claim covers only the named events and interval. It does not establish
scanner quality, eBPF readiness or authenticity of the source. This type is
separate from the existing capability scanning `common/v1` coverage.

### Reason-code ownership and vocabulary

Reason codes use `<owner>.<code>`, with lowercase letters, digits, underscores and
hyphens in each part; both parts start with a letter. The total length is at most
128 characters. AW reserves the `aw` namespace. External producers use their
agreed owner namespace, such as `agentsight`; namespace syntax does not establish
producer identity. AW maintains codes by field domain:

| Domain | Code | Meaning |
| --- | --- | --- |
| `association.gaps` | `aw.native_identity_unverified` | The available callback has not corroborated request identities |
| `association.gaps` | `aw.native_turn_unavailable` | The native turn identity is unavailable |
| `association.gaps` | `aw.instance_has_no_request` | An instance-level event has no request identity |
| `coverage.gaps` | `aw.coverage_unmeasured` | Observation coverage has not been measured |
| `coverage.gaps` | `aw.source_unavailable` | The observation source cannot currently be accessed |
| `coverage.gaps` | `aw.journal_corrupt` | Retained journal data fails decoding or integrity validation |
| `coverage.gaps` | `aw.history_incomplete` | Available history lacks an expected segment or terminal record; the cause is unconfirmed |
| `coverage.gaps` | `aw.history_missing` | Requested history cannot be located |
| `coverage.gaps` | `aw.retention_gap` | A known retention operation removed part of the requested history |
| `coverage.gaps` | `aw.consumer_lag` | The consumer has a known backlog relative to the source |
| Lifecycle `reasonCode` | `aw.native_adoption_unconfirmed` | Hook processing ended without native effect-adoption evidence |
| Lifecycle `reasonCode` | `aw.launch_failed` | Launcher startup or readiness failed |
| Lifecycle `reasonCode` | `aw.worker_failed` | The Hook event worker failed |
| Lifecycle `reasonCode` | `aw.expired` / `aw.stopped` | The event deadline or service stop ended processing |
| Lifecycle `reasonCode` | `aw.cancelled` | The turn was cancelled |
| Lifecycle `reasonCode` | `aw.owner_lost` | The execution owner was lost before a terminal outcome was confirmed |
| Lifecycle `reasonCode` | `aw.cancel_cleanup_pending` | Cancellation cleanup remains unfinished |
| Provider `failureCode` | `aw.provider_timeout` | The Provider did not complete within its deadline |
| Provider `failureCode` | `aw.transport` / `aw.exit` / `aw.incomplete_input` | Transport, process exit or incomplete input delivery failure |
| Provider `failureCode` | `aw.native_environment` / `aw.provider` / `aw.protocol` | Invalid native environment, structured Provider failure or protocol failure |

These codes define meanings for producers to implement. The journal/history and
consumer-lag codes prepare the public-audit and Sight integrations; this library
does not detect those conditions. New AW codes are reviewed in the relevant domain
table. A code's meaning remains stable; changing it requires a version decision.

The schema checks namespace syntax and accepts unknown codes for forward
compatibility. Consumers preserve unknown codes and present unknown gaps as
unclassified, retaining the declared association/coverage status. They must not
infer meaning from substrings or treat an unrecognized gap as complete coverage.
Association gaps describe identity evidence; coverage gaps describe observation
completeness. Consumers interpret each code in its field domain.

### Clock semantics

`occurredAt` comes from the native source's wall clock; `receivedAt` comes from the
AW producer's wall clock. Their clock relation is unspecified. Either ordering is
valid, including `occurredAt > receivedAt`; the difference is not a defined latency
or freshness measurement. `metadata.createdAt` is the producer's retained object
creation time, and `coverage.observedAt` is its coverage-assessment time.

The producer expresses both `coverage.interval` bounds in one clock domain and
ensures `start <= end` when both are known. Bounds that cannot be mapped to that
domain remain null, with a coverage gap. Comparing native event times against
these bounds requires an explicit clock mapping by the producer or consumer.

A semantic-arbiter bridge must establish evidence capture, decision and validity
times in its own agreed clock domain and enforce the evaluation budget there.
Copying `receivedAt` into a decision time or comparing UTC strings across sources
does not establish freshness or the `captured_at <= decision < valid_until`
relation. This contract carries descriptive timestamps; clock mapping and
evaluation deadlines belong to the integrating consumer.

## ExecutionEvent branches

`data` contains `name`, `subject`, `nativeRefs`, `association`, `times` and
kind-specific `details`:

| Name | Required association shape | Details |
| --- | --- | --- |
| `instance.started` | All request/native references null | Reported `pid`; descriptive, not process authentication |
| `instance.ended` | All request/native references null | `outcome`, `exitCode`, `signal`, `reasonCode` |
| `input.submit` | Session and turn; tool call null | `inputDigest` |
| `turn.ended` | Session and turn; tool call null | `outcome`, `reasonCode` |
| `tool.before` | Tool call; session/turn may be unknown | `name`, `nativeName`, `inputDigest` |
| `tool.after` | Tool call; session/turn may be unknown | Previous three fields plus `resultDigest`, `resultStatus` |

An `exited` instance has exactly one of exit code or signal, without a reason
code. Failed/incomplete instances carry a reason code and neither exit field.
Turn endings distinguish completed, failed, cancelled and incomplete; only
completed has a null reason. Tool result status is success/error/interrupted/denied.
A pre-tool event cannot carry a result. These fact names do not add Provider
Hooks, new native callbacks or capabilities to the Adapter matrix.

## AuditRecord identity and results

`data` contains `kind`, `eventRef`, `eventName`, `subject`, `chain`, `coverage`
and `details`. The event reference is an `ExecutionEvent` from the same producer,
including its scope and generation. The source `chain` contains `id`, a safe
integer `sequence`, and `sourceDigest` of the retained private envelope.

| Record kind | Details | Interpretation |
| --- | --- | --- |
| `event.observed` | Empty object | A captured event was recorded |
| `provider.completed` | `providerId`, `stepId`, `requestId`, `status`, `policyBlock`, `failureAction`, `failureCode` | A policy attempt completed; no native-adoption claim |
| `event.closed` | `outcome`, `reasonCode` | The event lifetime ended or remains incomplete; no implicit tool/turn success |

Provider status is `ok` or `failed`. Successful calls have no failure code/action;
`policyBlock` independently reports true/false or null when unknown. Failed calls
have a failure code, report/block/withhold_result action and null policy decision.
Post-tool records cannot claim an execution-preventing block; pre-execution
records cannot claim output withholding. Provider results apply only to
input.submit/tool.before/tool.after boundaries in this version.

`objects::audit_uid` computes SHA-256 over the canonical array:

```json
[
  "audit.aw/v1alpha1", "AuditRecord",
  {"producerId": "aw-1", "scopeId": "scope-1", "generation": "generation-1"},
  "chain-1", 1
]
```

The source generation is retained across reader restarts. Each source position
projects to at most one record in this version. Content changes at the same
position keep their UID and change their object digest, making conflicts visible.
Consumers use producer identity plus UID as the deduplication key and compare
digests. Hashes alone do not authenticate or prove persistence.

The runtime retains creation timestamps and deterministic fields across replay.
Readers retain the original source generation; a new service generation cannot
recover write authority for a previous run.

## Runtime production and projection

`aw-service` stores a validated `ExecutionEvent` in the existing Journal reservation's
`plan.executionEvent`. The source-chain key is its opaque UID. Provider completion
and closure records retain their creation time before the durable write. The
FileJournal format-1 envelope, digest chain and existing private fields retain
their meanings. A projection computes `sourceDigest` from the already retained
envelope, avoiding a self-referencing digest in the stored record.

`aw_service::public_projection::execution_event` validates the retained initial
object; `project_record` projects one already verified envelope. A missing initial
public object explicitly identifies a private or historical chain. Invalid public
objects return errors. Intermediates such as step dispatch and raw native-hook
completion have no public AuditRecord projection. Projection preserves producer,
UID, time, sequence and digest across repeated reads.

The audit directory contains a private `.aw-audit-source.json` descriptor with
exactly `apiVersion: audit.aw/v1alpha1`, `kind: AuditSource` and a random 64-hex
`sourceId`. This storage descriptor is separate from the registered public schemas.
It is atomically installed with file/directory synchronization, owned by the
service user, single-linked and mode 0600. The directory remains mode 0700.
`producerId` is `aw-service`, `scopeId` is the retained source ID, and `generation`
is the writing daemon's generation. Missing or invalid descriptors fail explicit
reads; service initialization creates a descriptor for a previously uninitialized
store without converting historical chains.

| Runtime boundary | Recorded fact and limit |
| --- | --- |
| `aw run` child spawn / wait | `instance.started` / `instance.ended`, even with evidence disabled; reported PID is descriptive |
| Configured input/tool Hook | Captured metadata and Provider completion; closure without native adoption remains `incomplete` |
| Qoder SDK / QwenPaw tool callbacks without policy steps | Default metadata routes record before/after facts without starting a Sight worker or retaining bodies |
| Native result | QwenPaw uses explicit ToolResponse state; SDK uses success/failure Hook types and explicit error flags; unknown states retain a gap |
| Managed invocation retirement | Removes callback deduplication detail after the existing invocation ledger fences replay |
| Unmanaged callback retention | At most 256 callback identities per instance; overflow records one durable `aw.observation_callback_limit` gap and rejects later new identities |

Missing required public metadata records `aw.native_metadata_unavailable` in a
private gap chain. Existing Scope lifecycle records retain parent/owner metadata;
Scope IDs never substitute for native turn IDs. Public tool subjects retain the
actual task instance and verified session/call association. Native references
preserve native IDs separately from AW-normalized invocation IDs. `turn.ended`
remains unproduced when the runtime lacks a confirmed native turn identity.

Coverage remains `unknown` with `aw.coverage_unmeasured`. A completed observation
chain acknowledges capture, while a completed Provider records its policy output.
Neither proves native effect adoption. Public metadata excludes input/result
bodies, environment, credentials and callback envelopes. Journal failure retains
the existing fail-closed service behavior; source reads never retry execution.

## Compatibility and integration

Existing configuration, Provider request/response, Evidence, Core contracts and
registered schema bytes/IDs remain unchanged. New private records add capture
metadata. Old readers can continue reading the format-1 Journal; old chains remain
explicitly without public projection. The projection maps private `status: error`
to public `status: failed` and keeps missing policy decisions distinct.

Unknown object versions and fields are rejected. Published definitions retain
resource IDs and exact bytes; shape or meaning changes need explicit versioning.
Rollback retains the extra metadata and source descriptor without converting or
replaying the historical chains.

Contract tests cover all six event shapes and immutable references. Service tests
cover disabled Sight, exact native association, deduplication and retirement,
unknown results, durable failure, concurrent source initialization and replay across
service generations. Native QwenPaw acceptance additionally checks the no-policy
facts while changing policies on the same Agent. Hosted Qoder Worker adoption and
Sight storage keep their separate [integration acceptance](../integrations/agentsight/agentsight-integration.md).
