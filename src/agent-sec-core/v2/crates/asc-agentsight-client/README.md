# AgentSight Client and Reconciler integration

`AgentSightClient<T, R>` implements `asc_policy_target_contracts::TargetDeploymentClient` directly.
The composition root registers `AgentSightClientFactory::default()` in the
Reconciler's `TargetDeploymentClientFactory` registry. Registration performs no
credential or network I/O. Each attempt opens a Client, reads the current token
file and reuses that instance for preparation and create/update, or deletion.
Missing/unreadable/invalid credentials are retryable with sanitized codes
`AGENTSIGHT_CREDENTIAL_UNAVAILABLE` or `AGENTSIGHT_INVALID_CREDENTIAL`. Invalid
endpoint configuration is rejected as `AGENTSIGHT_INVALID_BASE_URL`. Credential
changes take effect on the next attempt; retry remains bounded by the core policy.
Tests can inject closures returning scripted Clients. No PEP-specific logic is
added to the Reconciler, and the Client never reads or writes a repository.

## Operations

| Client API | Behavior |
|---|---|
| `prepare_apply(plan)` | Decode the existing Adapter plan; derive UUIDv5 from Binding ID/revision; read boot ID and process start time; encode a fixed, non-secret request and cleanup reference. No HTTP. |
| `create(prepared)` | Validate format, digest, target reference and process identity; check PEP capability; recheck identity; send the exact saved request bytes. |
| `update(previous, prepared)` | Validate/preflight before deleting old enforcement; delete old targets; create only when all old targets are confirmed absent. Return partial observations on any failure. |
| `delete_targets(targets)` | Delete saved target references without reading the current Binding, PID or boot ID. Only 204 or typed `binding_not_found` 404 confirms absence. |

The trait's `delete(targets)` delegates to `delete_targets`. All deployments use
the prepared-replay path; the former `apply(plan)` and
`delete(binding_id, revision)` convenience APIs have been removed. Callers must
prepare a call-local request and register its target before `create` or `update`, then pass saved
target references to deletion. The former convenience-result enum
`AgentSightDeploymentState` is now private; mutation results use
`DeploymentReport`. The HTTP protocol is unchanged. The unreleased cleanup schema
remains version 1 and contains only Binding ID/revision for deletion.

The default target route is `agentsight`. `with_reconcile_route("host-primary")`
selects another bounded non-secret configuration identity. The registry key must
match it. The composition root must not reuse a route for a different PEP
endpoint while saved targets refer to it; credential changes alone do not change
the target identity. A wrong route/ID/cleanup tuple is rejected before HTTP.

## Prepared request contract

Adapter plans include `source.scopeRevision` alongside `source.scopeId` as
provenance. The Client validates a supplied revision but does not use it in the
target UUID, cleanup input or HTTP request. Earlier v1 plans without this field
remain readable; new Adapter output always supplies it. The plan format remains
`agentsight.actplane.binding.v1`; update the strict Client reader before emitting
the added field. Already prepared requests are unchanged.

`source.policyId` and `source.policyRevision` populate AgentSight's product-policy
attribution fields. Deployment identity remains the UUID derived from the source
Binding ID/revision, independently of Policy and Scope attribution.

New requests send `agent_id: ""` because the product Binding supplies no Agent
identity. Scope identity remains plan provenance and is never substituted for
Agent identity. The generic `/api/enforcement/bindings` endpoint accepts this
empty string; the separate file/credential convenience endpoints have different
validation rules. Existing prepared requests keep their saved attribution and
exact bytes on replay, preserving the target's request identity contract.

Production endpoint configuration accepts HTTP only for literal IPv4 loopback
addresses or IPv6 `::1`. All other hosts require HTTPS. Use `127.0.0.1` or `[::1]`
instead of `localhost` for local HTTP, avoiding reliance on name resolution.
Certificate verification remains enabled and redirects remain disabled.

Format: `agentsight.enforcement.apply.v1`. Opaque content contains schema version
1, boot ID, exact POST body bytes and their SHA-256 digest. Cleanup contains only
schema version 1 and Binding ID/revision. It does not store process identity or
request digests. Prepared content is call-local and is not persisted.

Each retry prepares against the current process identity. Cleanup does not compare
that identity with a previous attempt. The checks below protect the identity
captured by this particular preparation, not identity continuity across attempts.

Replays check boot ID and `/proc/<pid>/stat` start time without regenerating the
body. PID reuse, process exit or a different boot rejects Apply. Temporary
identity-read failure remains retryable. Identity is checked again after health
or old-target deletion, since either can take time. AgentSight's own process
identity validation remains necessary for the final check-to-use window.
Valid alternate UUID spellings of the same boot ID compare equal. Malformed or
nil UUIDs are invalid prepared payloads; normalization never rewrites POST bytes.

`ProcessIdentityResolver` requires explicit implementations of both process start
time and boot identity. There is no default failure or unguarded apply path.
`ProcProcessIdentityResolver` implements both. It distinguishes a vanished
`/proc/<pid>/stat` as `Exited`,
reported as `AGENTSIGHT_PROCESS_EXITED`; callers exhaustively matching that enum
must include the new variant.

## Failure and update semantics

The Adapter generates DSL without embedding an ActPlane compiler. The Client's
preflight checks request integrity, process identity and target health/capability;
it does not prove that the deployed target accepts the DSL. The current Client
has no remote validate-only call, so compiler rejection is learned from Apply.
In particular, update can confirm old-target deletion before the new DSL is
rejected. Validation before cleanup would require a target validation contract
using the deployed compiler, not a separately pinned local compiler.

The return value includes per-target Present/Absent/Unknown and a bounded safe
error. A timeout, malformed response, rejection or unconfirmed deletion never
silently clears a target. For example, after old A is deleted and creating B
fails, the report retains `A=Absent, B=Unknown` and the error. The core records
both facts before changing lifecycle status. No automatic rollback is attempted.
DELETE absence depends only on 204, or 404 with `error.code = binding_not_found`;
the `retryable` field is irrelevant to that observation. For other HTTP failures,
missing `retryable` defaults to false; 429 and 5xx still select retryable behavior.

`previous` must exclude the new target and contain no duplicate identities.
Every cleanup reference is validated before any modifying request; validated
UUIDs are reused throughout that operation. An invalid later reference therefore
cannot cause partial deletion of earlier valid references.
After partial cleanup, the core may retry `update([], prepared)`; this posts the
same new request and never deletes the new target first. A failed old deletion
prevents POST, but already-confirmed deletions are still returned.

## Executable evidence

From `v2`:

```sh
cargo test -p asc-agentsight-client --locked --offline
```

`tests/client.rs` tests request identity, wire errors, capability gating and exact
absence signals through the prepared APIs. It shares the `Wire` and `Identity`
test ports with `tests/prepared_client.rs`, which compares complete frozen
prepared artifacts and exercises replay, partial errors, validation and cleanup independently of the
core. The shared ports are in `asc-policy-target-contracts`; data types are in
`asc-policy-types::target`. This crate has no runtime or test dependency on
`asc-pcp` or its repository.

The Reconciler crate owns the subsequent `agentsight_integration` suite, which
connects Adapter, Reconciler, Client, Ureq and memory repository to an HTTP mock
and checks exact requests and registration before modification.

This establishes real Client/transport wiring, not live AgentSight or kernel
enforcement. PAP notifications and worker timers live in `asc-policy-runtime`; the daemon
composes them when target credentials are configured. SQL recovery and full
real-process E2E remain separate work.

`tests/factory.rs` verifies deferred credential loading, retryable credential failures,
and actual HTTP authorization using separate Client instances before and after
a token-file update. No daemon/CLI token configuration option is added.
