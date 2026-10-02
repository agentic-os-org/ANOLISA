# Binding reconciliation core

`BindingReconciler::reconcile(binding_id, &mut schedule)` owns one synchronous, due attempt.
Every call reads the latest Binding status/error and deployment records. Retry
progress is an AttemptSchedule owned by the caller between calls, never stored. Apply
translates and prepares afresh; Delete uses recorded targets without translation.
The Client registry contains `TargetDeploymentClientFactory` ports. A due Apply
opens one Client after translation and reuses it through prepare and create/update.
Delete opens one Client per saved route. Initialization failures follow normal
attempt bookkeeping and retry classification; no Client is opened for skipped work.
There are no cross-call plans, prepared requests, pending outcomes or step cursors.

The caller must serialize attempts for each Binding in the same Repository.
In production one Runtime WorkQueue owns this responsibility: its Running entry
spans Client I/O, result bookkeeping and panic unwind. Repeated notifications only
mark the entry dirty. The core has no execution lock or shared lock registry;
callers must not bypass the queue or start independent queues over the same store.
Blocking calls must be joined even if an async waiter times out.

The call-local `ExecutionSlot` retains only an outcome and a pending write receipt
for panic bookkeeping. It is discarded on return or unwind and cannot survive a
process crash.

## Storage and execution

[`asc-policy-repository`](../asc-policy-repository/README.md) supplies consistent
aggregate reads and field-scoped conditional writes. `ReconciliationPatch` has no
spec field. Registration writes deployments only; completion merges observations
and conditionally advances the claimed revision/status. A stale Apply can record
valid target observations while preserving a newer Delete and its retry budget.

Target references and UNKNOWN responsibility are registered before modifying I/O.
Only confirmed Absent targets are removed. Delete removes the whole aggregate
atomically after all targets are confirmed absent. Spec revisions change only
with spec, never for Delete or reconciliation status.

Temporary preparation belongs to this call. Existing targets select `update`;
its `previous` slice excludes the new target and may be empty after partial cleanup.
A same-revision target whose opaque cleanup reference differs from its registration
is rejected with `RECONCILE_TARGET_IDENTITY_CHANGED`, preserving cleanup records.
This compares cleanup parameters, not process identity. AgentSight checks the
identity captured by each new preparation; it does not retain the prior process
identity across attempts.
Cross-route Apply migration is unsupported: it returns
`Rejected / RECONCILE_TARGET_UNAVAILABLE`, records ApplyFailed without a retry
deadline and preserves old targets without create/update I/O.

## Retry and failure

| Return | Runtime action |
|---|---|
| Completed / Failed | Remove scheduling entry; Failed records remain |
| RetryAt | Wait without holding a worker |
| Superseded | Wait, then check the latest intent within the queue retry budget |
| Skipped | Recheck pending deadline or remove a missing/terminal entry |
| StoreError | Log the error and schedule a bounded retry for this Binding |

Illegal completion/retry/failure status transitions return `StoreError::Invalid`
without constructing a write. They do not close admission for other Bindings.
`clock()` shares the core's clock instance with the Runtime; retry deadlines and
timer checks must use that same clock domain.

A failed result transaction does not retain its result after the call exits.
On the next call, saved terminal state is respected. A remaining running state is
recovered only after the caller has ensured the prior local call has exited. Recovery preserves consumed attempts, applies backoff or exhausts the
budget, and retains every possibly-present target. It cannot undo a newer Delete.
The next due attempt starts from scratch and may repeat remote I/O safely.

Within a call, completion keeps its exact write ID for bounded contention retries
and panic cleanup. Unwind tries to submit an available result, otherwise records
`RECONCILE_WORKER_PANICKED`; it then propagates to the owning worker, which catches
the attempt panic and continues other Bindings. Unconfirmed outcomes stop automatic
execution of that ID until a new notification. No result or
write survives the call. Unwind recovery does not prove process-crash
recovery or cross-service fencing.

## Integration and validation

[`asc-policy-runtime`](../asc-policy-runtime/README.md) supplies the queue, workers,
timers and compensation scan. PAP notifies after Binding admission; daemon
composition supplies the concrete AgentSight Adapter/Client. Shared ports live in
`asc-policy-target-contracts`, below both core and concrete Clients.

From `v2`:

```sh
cargo test -p asc-pcp -p asc-policy-runtime --locked --offline
```

The [acceptance standard](../../fixtures/reconciliation/ACCEPTANCE.md) covers
complete serialized records, dependency inputs/results and ordered traces. Tests
also compose real PAP/memory and actual Adapter/Client/Ureq with a loopback mock.
Storage is process-local memory. Full CLI/daemon E2E is a separate PR; SQL,
crash/restart error injection and live PEP/kernel enforcement are separate gates.

The Runtime holds AttemptSchedule only while the queue entry exists. Automatic
retries preserve its attempt count and deadline; a new explicit Pending request
without an error resets progress. Rebuilding the queue starts with fresh progress,
including a new delay for interrupted running work. Failed states remain terminal.
Claim atomically changes the phase and clears the previous status.error.
