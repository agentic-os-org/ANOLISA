# PEP policy selection in AgentSecCore

[中文版](CEDAR_PEP_SELECTION_zh.md) · [Unified Cedar policy management and deployment](CEDAR_PRINCIPAL_POLICY_DEPLOYMENT.md)

The v1/v2 eligibility and migration rules below refer to the original selection rollout. The [single-revision contract](CEDAR_PRINCIPAL_POLICY_DEPLOYMENT.md#single-revision-contract) describes placement for newly authored and migrated policies.

This design is for developers implementing policy consumers. Baseline `d41cede90` provides Cedar v3 automatic deployment. This branch adds a shared `select` interface for three PEPs. PAP retains the complete authored source. Each PEP selects the statements and branches it consumes and reuses the result for that policy-set generation. [Selection types](../../v2/crates/asc-policy-cedar/src/selection.rs) · [Shared cache](../../v2/crates/asc-pap/src/selection.rs)

## 1. How each PEP consumes one rule

An author wants to forbid Qoder from writing, deleting or renaming entries at or below `/work/reports`.

```cedar
forbid (
    principal,
    action == SecCore::Action::"file.write",
    resource is SecCore::File
)
when { principal has agentType && principal.agentType == "qoder" }
when { resource.path == "/work/reports" || resource.path like "/work/reports/*" }
when {
    context.input.operation == SecCore::WriteOperation::"write" ||
    context.input.operation == SecCore::WriteOperation::"delete" ||
    context.input.operation == SecCore::WriteOperation::"rename"
};
```

The three operations share a subject and a path condition. Selecting any subset preserves the `forbid`, Qoder condition, path and applicable annotations. Only the final `when` changes.

| PEP | Selected operations | Actual downstream input and effect |
|---|---|---|
| Sandbox | `write` | The PDP loads the Cedar statement with the ordinary-write branch alongside other applicable permits and forbids. Even with a general write permit, a Qoder request to write `/work/reports/q1.txt` is denied. Sandbox constructs file-write requests at launch with operation `write`. |
| ActPlane | `write`, `delete` | The automatic Scope stores the complete two-branch fragment. When the daemon matches Qoder, the compiler reads that fragment and produces ordinary-write and deletion protection. Its input excludes `rename`. |
| Gateway | Empty, outside its HTTP domain | This file statement contributes nothing to the credential plan. For HTTP statements, Gateway reads the selected fragments and assembles each Scope using the existing subject, credential grant and restriction rules. |

No PEP claims the general `rename` prohibition in this example. ActPlane deletion protection can also block a rename of an ancestor to keep paths stable, a stricter effect. Directory-tree ordinary-write protection cannot mediate every modification through a shared writable mapping, a weaker effect. Both describe conditional capabilities. Receipts or events establish whether a particular process was attached or an operation was blocked. [Sandbox requests](../../v2/crates/asc-pdp-runtime/src/request.rs) · [File enforcement boundaries](../../../agentsight/docs/design/actplane-file-enforcement-profile.md)

## 2. Interface contract

```rust
type Select = fn(&ValidatedStatement<'_>) -> Selection;

struct Selection {
    source: StatementRef,
    selected: SelectedStatement,
    deviations: Vec<EnforcementDeviation>,
}
```

- **Input.** One complete statement validated against its original schema, with Policy ID, Policy revision, statement ID, schema reference, conditions and annotations. A given implementation makes the same selection for the same input. The consumer verifies the process, request, available credentials and published Scope later.
- **`source` identifies the author’s statement.** Policy ID, revision and statement ID identify a single origin.
- **`selected` supplies executable input.** A whole statement may reference its pinned source. A partial selection returns a complete, independently loadable Cedar fragment and its branch locations in the source. Sandbox loads that fragment directly. Deployment stores it in the automatic Scope snapshot and discharges only the subject conditions verified by that Scope for the Binding. An empty result distinguishes an unrelated statement from a relevant unsupported one and gives a reason for the latter. Restrictions that might affect a grant still reach backend admission.
- **`deviations` describes known enforcement differences.** The adapter supplies the source branch, direction (stricter or weaker), reason code, condition and explanation. A future management client can aggregate coverage and differences by source. If a statement can only be evaluated whole and its coverage cannot be partitioned, report undetermined coverage. This phase supplies the return contract only. The query API and UI are deferred.

Only split branches when their meaning is preserved. Selecting deletion from “write A or delete B” must retain “delete B.” For a potentially relevant statement that cannot be split safely, Sandbox keeps the whole statement with its guards, `unless` clauses and annotations, then applies the existing path check and Cedar evaluation. Preserve the load error when valid Cedar cannot produce the required OS permissions. For v1/v2, first apply existing whole-Policy principal-header eligibility. Rules assigned only through a manual Scope stay in that snapshot. [Legacy eligibility](../../v2/crates/asc-policy-cedar/src/lib.rs) · [Path checks](../../v2/crates/asc-pdp-runtime/src/path_conditions.rs)

## 3. Cache and publication

The shared caller keeps an in-process cache keyed by **PAP policy-set generation and PEP type**. Each PEP reuses its own static selections. It only needs the current selection in memory. The PDP or Scope snapshot holds published execution state.

1. Increment the generation in the transaction that commits a Policy creation, update or deletion. Read it cheaply on its own. A cache hit reuses the selected fragments or the PDP loaded from them. On a miss, read the generation and sources from a consistent snapshot, share parsed source for that snapshot and run one full selection pass for the requesting PEP. Store the result under the generation actually read.
2. Concurrent loads for the same PEP and generation share one build. Retain the per-PDP refresh lock and the deployment source-revision check at publication. A late build cannot replace a newer active version. The generation only signals cache invalidation. The Sandbox receipt’s `policy_version` continues to identify the authorization policy set. Failure to read or load the current set returns the existing error.
3. Sandbox evaluates each request. ActPlane matches and attaches each new instance, verifying its UID when required. Gateway compiles per Scope and switches after successful admission and publication. HTTP requests use the published plan. If a running Gateway rejects a candidate at admission, existing and newly matched processes continue with the serving version. After restart, rebuild the in-memory cache and verify automatic credential routes through existing recovery before restoring traffic.

Back up the SQLite policy database before upgrading. On first startup the repository upgrades version 1 to 2 and initializes the policy-set generation. If schema or stored-policy validation fails, the transaction rolls back and the database stays at version 1. After a successful upgrade, rolling back to the previous binary requires the matching pre-upgrade backup. [Database migration](../../v2/crates/asc-policy-repository-sqlite/src/migration.rs)

## 4. Which version a retry uses

| Work being retried | Version source | Example |
|---|---|---|
| Installation for an already published Binding | Use its pinned execution snapshot and verify that the work still targets the current Binding revision. The reconciler discards superseded work. | After an ActPlane installation failure, retry the original two branches if the assignment remains valid. Replacement or revocation prevents the old work from reinstalling them. |
| Automatic `scope retry` by an operator, or a worker retry of an unpublished candidate | Check the **current** PAP policy set, reuse that generation’s selection, assemble the candidate and run admission again. An initial failure can leave only a candidate ID. | Gateway serves v1 (GET). Version 2 adds POST but fails admission. Retry v2 after fixing the fault. If PAP now holds v3, attempt v3. If the source was deleted, follow withdrawal. |

[Automatic Scope retry](../../v2/crates/asc-pap/src/service.rs) · [Retry after a rejected first publication](../../v2/crates/asc-pap/tests/automatic_scopes.rs)

## 5. Acceptance cases

| Case | Action and observable result |
|---|---|
| Fragment reaches the consumer | Submit the policy in section 1. Inspect PDP input for ordinary write, ActPlane compiler input for ordinary write and deletion, and empty Gateway input. With a general permit, compare actual Sandbox decisions for Qoder and another agent type. For “write A or delete B,” verify that each operation stays paired with its path. |
| Reuse across all three PEPs | Start cold and count selection passes per PEP for one generation. Issue 1,000 Sandbox requests and repeated HTTP requests, match multiple Qoder processes and plan multiple Scopes. Each PEP using that generation performs one full pass. Verify that individual instances and requests still receive their own checks. With credential Scopes for UIDs 1000 and 1001, a POST restriction for 1001 enters only its candidate. |
| Legacy scope and receipt | Bind a legacy v2 file prohibition manually to one PID. Change that manual Policy and verify that other processes retain their Sandbox decisions and the same authorization-set version in their receipts. |
| Update during a concurrent load | Use v7 as the only permit to read the test file. Pause its load with a barrier, commit v8 removing the permit and issue new requests before releasing the old load. All requests started after commit are denied using the authorization set for v8. Concurrent requests for one generation trigger one selection pass. Separately delay an old deployment candidate until after a newer publication and verify that it cannot restore the previous grant. |
| Retry and recovery | Trigger both failures from section 4. Verify the Binding revision and fragment, and observe real Gateway GET/POST results before and after successful publication. Cover a rejected first admission with no saved snapshot, a source updated or deleted before retry, and restart requiring revalidation. |
