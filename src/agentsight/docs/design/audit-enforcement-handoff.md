# Audit-to-Enforcement Handoff

## Goal

Upgrade one credential-exfiltration audit case to temporary kernel enforcement without exposing a
runtime gap, losing the original case identity, or discarding evidence history. Expiry and explicit
removal restore the exact audit policy that produced the case.

## Existing contracts

This design extends the reviewed Risk Enforcement, System Audit, and containment layers:

- every apply requires the acknowledged required-violation subscription generation;
- each ordinary UDS request has one absolute connect/write/read deadline;
- containment identity is the stable Agent, session, PID, process-start, policy, and evidence tuple;
- `public_ipv4` is the only supported destination scope and remains fail closed;
- omitted, null, and numeric duration values remain distinct;
- detach failure number five is terminal;
- only an enforced acknowledgement proves policy attachment and only `blocked=true` proves denial.

The System Audit case and its evidence are immutable. Handoff appends lifecycle history to the same
case; it never creates a second case for the enforcement binding.

## Considered approaches

### Detach then apply in AgentSight

Two ordinary UDS calls are easy to add, but a crash, reconnect, or stale generation may interleave
between them. The process may briefly have no audit or enforcement policy, and recovery cannot tell
which binding should own the singleton runtime.

### Leased compare-and-replace with durable intent

AgentSight persists a directional transition, then sends one leased replace command. The enforcer
prevalidates both policies and permits ownership transfer only when the backend can prove no-gap
semantics. This is the selected protocol because the singleton owner is the only layer that can
exclude standalone apply and detach operations for the complete swap. Backends without a suitable
kernel primitive retain the source and leave the transition retryable.

### Multiple simultaneous bindings

Keeping audit and enforcement active together would avoid a swap, but the pinned ActPlane profile
owns one runtime binding. Supporting concurrent policies requires a broader ActPlane redesign.

## Replacement protocol

Protocol version 4 adds typed replacement requests and outcomes. A replacement contains the exact
enforced source snapshot, optional structured source-policy provenance, and either a generic
`ApplyPolicy` target or a product-level `ApplyCredentialPolicy` target. Source and target must
match Agent, session, root PID, and process-start identity. An applied acknowledgement must match
the target and preserve the source runtime domain. Invalid identity, policy, or domain evidence
fails before runtime mutation.

Mutation is exposed only through:

```text
replace_policy_leased(request, required_subscription_id)
```

The legacy unleased command is rejected. The service holds the required-subscription registry lock
across backend replace, checks the peer before and after it, and serializes detach through the same
lock. If the required peer disappears after target attachment, the service attempts the exact
reverse replacement before returning a lease error. AgentSight invalidates the rejected generation
and does not persist a successful transition.

Outcomes contain stable codes rather than backend text:

- `Applied`: the replacement owns the runtime;
- `SourceRetained`: validation failed before detachment;
- `SourceRestored`: target attachment failed and rollback restored the source;
- `Conflict`: a third binding owns the runtime and remains untouched;
- `Indeterminate`: neither source nor target ownership can be proved.

## ActPlane no-gap boundary

The pinned ActPlane ABI cannot prepare a second authoritative profile or atomically swap a live
domain's profile at a kernel/map boundary. A lifecycle mutex excludes competing control-plane
calls but cannot prevent a workload from running between clear and install operations. PID/start
snapshots also cannot cover PID reuse and children joining after the snapshot.

The real adapter therefore validates exact source ownership and fully compiles the target while
the source remains active, then returns `SourceRetained` before any runtime or map mutation. It
does not clear policy state, unbind the source domain, rebind members, or claim atomic success.
Target-already-authoritative remains idempotent only when its acknowledgement proves the original
runtime domain. A third binding and an empty runtime remain conflicts. Real handoff can be enabled
only after the pinned ABI supplies a provable atomic swap or overlap primitive.

## Durable transition log

`enforcement.db` stores one transition for each `(containment action, direction)` pair. The record
contains the immutable replacement request, phase, typed acknowledgement, stable failure code, and
update time. Forward and reverse records preserve the complete history.

The coordinator persists intent before UDS mutation. It snapshots the current ingestion generation,
uses that subscription ID for the leased replace, performs the post-replace combined-health check,
then holds the generation guard while one SQLite transaction:

- marks the source detached;
- inserts or updates the target acknowledgement;
- completes the transition.

Persistence cannot report replacement success before both remote acknowledgement and local
generation fencing succeed. Transport loss, stale lease rejection, source retention/restoration
during a reverse attempt, health degradation, conflict, and indeterminate outcomes remain
retryable and block ordinary UUID-ordered reconciliation. Recovered acknowledgements are fenced
against the exact adopted subscription generation immediately before persistence. Unfinished
transitions are resumed before standalone desired bindings.

## Containment orchestration

Each new containment action stores the exact source audit binding ID. Forward transition creation
uses the already-reviewed case provenance check, then replaces that source with a credential policy
that retains Agent, session, policy, revision, canonical source path, trusted endpoints,
`public_ipv4`, PID, and process-start identity. Atomic handoff cannot retarget another process.
The action becomes active only after the completed transition acknowledgement is persisted under
the same readiness generation. Reverse construction restores the original structured credential
policy, including normalized audit-event behavior. Explicit removal targets the requested action
directly and never falls through to generic detach for a handoff-managed binding.

Recovery never infers provenance from an arbitrary detached binding. Legacy pending rows without a
source binding fail closed. A transition that completed before action activation is reused instead
of creating another binding or case.

Expiry and explicit removal first move the action to `Expiring`, then create or resume the reverse
transition from the completed forward record. The reverse request uses the acknowledged containment
binding as its source and the original audit request as its target. The action becomes `Expired`
only after the audit acknowledgement is durable under the generation guard. Failed restoration
keeps the action retryable; attempts one through four back off and attempt five is terminal.

The existing enforcement DELETE endpoint recognizes a live containment binding and delegates to
this expiring path. Non-containment bindings keep the standalone detach behavior. No Dashboard
layout or Security Observability route changes are required.

## Restart and stale acknowledgement behavior

| Crash or race | Convergence |
| --- | --- |
| Intent stored before UDS replace | Resume the exact transition. |
| Backend target active before persistence | Target acknowledgement completes idempotently. |
| Source active after target rejection | Record source-restored without creating a target row. |
| Empty backend after interrupted swap | Install the transition target under the lifecycle lock. |
| Third binding active | Preserve it and report indeterminate conflict. |
| Required subscription changes during replace | Reject the stale result; reverse remotely when possible. |
| Old acknowledgement arrives after a newer generation | Generation guard rejects persistence (ABA-safe). |
| Reverse target active before action update | Resume reverse, then mark the action expired. |

## Error and exposure boundaries

Raw UDS errors, filesystem paths, policy DSL, kernel details, and sensitive evidence remain outside
API responses and transition failure fields. Stable replacement codes and existing sanitized
containment messages are the only persisted operator detail. Store failures retain the existing
fixed public error.

## Verification

Portable tests cover protocol serialization, leased command rejection, service generation loss,
mock source/target/third-party states, structured-policy restoration, delayed event attribution,
store atomicity, transition ordering, action identity, and explicit-removal routing. ActPlane unit
tests cover fail-closed source retention for PID reuse and concurrently joining process members.

Linux validation must additionally run the full enforcement and containment pipelines, apply all
ActPlane patches in order, compile the real backend, and prove the audit → blocked enforcement →
restored audit story on a supported kernel.
