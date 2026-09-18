# Binding reconciliation repository contract

Shared data and storage ports sit below PAP's memory repository, Reconciler and
Policy Runtime. This crate has no Client calls, execution locks or scheduling logic.

| Operation | Contract |
|---|---|
| `get_binding_state(id)` | Consistent Binding/spec, status/error and deployment snapshot |
| `compare_exchange_binding_state(expected, write)` | Atomic conditional reconciliation patch or aggregate removal |
| `scan_reconciliation(after, limit)` | Bounded page of Binding metadata ordered by stable ID; scheduler filters pending/running |

`BindingStateWrite.next` contains `ReconciliationPatch`, with optional status
and deployments fields. It cannot carry or write spec. `None` removes
the Binding and its related records atomically. The `new(snapshot)` convenience
constructor selects only reconciliation fields; spec remains owned by PAP.

A write compares Binding revision/phase and only the status explanation/deployments
it updates. Thus a deployment-only write preserves a concurrent diagnostic update;
it does not require a global resourceVersion. Aggregate removal compares all
reconciliation fields. Reads and writes share the authoritative PAP Binding map.

The latest write receipt acknowledges an exact replay within a call, including
a post-commit panic. Reusing its ID with different contents is invalid. Missing
IDs acknowledge removal but never allow replacement to recreate a Binding. No
permanent tombstones or historical receipts are retained. The core re-reads and
recalculates conflicts; it does not retain a pending write across calls.

`BindingView.status` contains `phase` and an optional bounded `error` (kind/code).
There is no RuntimeState: attempt count and retry deadline belong to WorkQueue,
while retry policy comes from configuration. None of them is persisted.
Plans, prepared requests and intermediate Client results are absent from storage.
Deployments retain target identity/cleanup and observations, including UNKNOWN
responsibility. AgentSight cleanup contains schema version and Binding ID/revision
only; process identity and request digest remain in call-local prepared data.

The implementation is process-local memory. Rebuilding WorkQueue resets counts
and deadlines; a future SQL implementation retains status/error and deployment
responsibility, not retry progress. See the
[runtime design](../../../docs/design/BINDING_RECONCILER_RUNTIME_DESIGN_zh.md).
