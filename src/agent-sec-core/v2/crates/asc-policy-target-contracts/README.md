# Policy target contracts

PEP-neutral Rust ports shared by the Reconciler and concrete Adapter/Client
implementations. This crate depends only on `asc-policy-types` at runtime;
it contains no Reconciler, repository, transport or PEP implementation.

- `TargetBindingAdapter`: translate a complete `PreparedBinding` into an opaque
  `TargetBindingPlan`; distinguish semantic rejection from an internal failure.
  Closures implement this port, so an existing pure Adapter needs no wrapper crate.
- `TargetDeploymentClientFactory`: open an attempt-local Client with classified
  failures; registering a factory performs no I/O. Closures implement this port.
- `TargetDeploymentClient`: prepare stable replay/cleanup input without target
  modification, then create/update/delete and report per-target observations.

Data contracts live in `asc-policy-types::target`: `TargetRef`, `PreparedApply`,
`DeploymentReport`, `Observation`, `Presence`, `Failure` and `FailureKind`.
These are not AgentSight HTTP request types. UUID generation, DSL, request encoding
and cleanup bytes remain specific to each implementation. Repository runtime,
deployment bookkeeping, revision/status CAS and execution locks do not belong here.

`prepare_apply` may read local process identity but must not modify a target. The
caller registers the target reference before create/update; the prepared body is
call-local. Each reconciliation retry prepares again. Cleanup contains parameters
needed to delete the target, not an execution checkpoint. Calls are synchronous and bounded
by concrete Client timeouts. Local work must finish before return; remote late
completion requires a separate cross-service protocol. `update` owns PEP-specific
replacement semantics and receives historical targets excluding the new identity.
Partial failure preserves observations; only confirmed absence clears a target.
Routing and matching Adapter/Client formats are composition responsibilities;
supporting multiple implementations does not promise multi-PEP atomic deployment.

## Acceptance and compatibility

Acceptance type: `GREENFIELD_CONTRACT`; no V1 runtime dependency. This extraction
introduced shared Rust imports without changing serialized artifacts. The Client
registry now requires factories; consumers inject a factory closure or a concrete
PEP factory. There is no serialized state or HTTP protocol change.
`asc-pcp` can re-export the ports/data for its existing local consumers.

From `v2`, run `cargo test -p asc-policy-target-contracts --locked --offline`.
Three tests freeze complete serialized replay/partial-result artifacts, bounded
errors and identity, and object-safe Adapter/Client use without a Reconciler or
concrete PEP dependency. Client tests provide direct-consumer behavior evidence.
The Reconciler integration slice additionally validates registration and lifecycle
ordering; mock tests do not establish persistence or live PEP enforcement.

Rollback must revert consumers and the shared definitions together; do not delete
this crate while Client imports still reference it. No data migration is required.
