# AgentSight Adapter: Cedar file protection

[中文版](AGENTSIGHT_CEDAR_TRANSLATION_zh.md)

This document defines the AgentSight Adapter contract, including its Translator,
Deployment Client and backend enforcement limits.

AgentSecCore translates Cedar file-deletion and ordinary-write restrictions into
AgentSight's structured file-protection payload. This extends the Cedar admission
and persistence implementation at `af1bc18a9`; native JSON policies retain their
existing DSL path. The production daemon already registers `AgentSightTranslator`
and `AgentSightClientFactory`, so both sources use the same deployment lifecycle.

## 1. Contracts and versions

| Boundary | Contract |
|---|---|
| Cedar source | `seccore.system`, revision `1`; original source stays in Policy and Scope/Binding snapshots |
| Translator input | `PreparedBinding`; Cedar source is checked and converted with `asc_policy_cedar::pst` |
| Target plan | `agentsight.actplane.binding.v2`, `schemaVersion: 2` |
| Structured artifact | `application/vnd.agentsight.file-protection.v1+json`; `content` is serialized JSON text |
| Sight HTTP request | `POST /api/enforcement/bindings`, `policy_mode: "enforce"`, `file_protection_policy.version: 1` |
| Prepared request | Existing `agentsight.enforcement.apply.v2`; exact request bytes and process identity |

The v2 deployment plan and the version-1 file-protection payload are different
contracts. This change does not introduce native authoring `specVersion: "0.2"`
or change SQLite's version. The Client explicitly supports file-protection
payload version 1. Its payload types, version and media type are defined once in
SecCore's `asc-agentsight-protocol`, which the Translator and Client both use.
The crate depends only on `serde`; fixed JSON fixtures check the wire contract.
Adding a payload version requires an explicit Translator/Client change and
compatibility validation against the target release. A structured request omits
`policy_dsl`; legacy requests retain their existing serialization and replay bytes.

This target consumes **additional forbid constraints**. A matching rule blocks
the covered operation; otherwise this policy adds no restriction. It does not
execute Cedar Authorizer's default-deny algorithm. `permit` is rejected by this
Translator, including in mixed sets; the common Cedar frontend still accepts it
for other consumers. There is no implicit permit-all, permit-to-exclusion rewrite,
or general Cedar authorization service in this path.

## 2. Supported fields and expressions

| Input | Translation |
|---|---|
| Unconstrained `principal` | Scope/Binding selects the process instance and process tree |
| `action == SecCore::Action::"file.write"` | Accepted; `action in [...]` must be a nonempty list containing only that action |
| Unconstrained resource or `resource is SecCore::File` | Accepted |
| `context.input.operation == SecCore::WriteOperation::"delete"` | `delete: true` |
| `context.input.operation == SecCore::WriteOperation::"write"` | `ordinary_write: true` |
| Operation OR / literal-set `contains` | Preserve each operation's path conditions; combine flags only for identical targets and exclusions |
| `resource.path == "/data/a"` | `exact_file`; the target must be an existing regular file at deployment |
| `path == "/data" || path like "/data/*"` | `directory_tree`; the root must be an existing directory |
| Literal path-set `contains`, path OR | Multiple targets |
| `!=`, `!`, `unless` within a bounded path range | Exact-entry or directory-subtree exclusions where representable |
| `&&`, `||`, Boolean constants | Bounded Boolean lowering, preserving intersections, unions and operation/path association |

Equality operands and the two arms of the directory-tree expression may be
reversed. The root-equality/descendant-pattern pair must be an OR subtree; this is
a documented syntax subset, not a promise to recognize every equivalent Cedar
expression. The special root spelling uses `path == "/" || path like "/*"`.
Negative operation tests are accepted only when the final operation set is
restricted to write/delete; referencing create/rename/setattr is rejected.

Paths must be absolute literal paths without NUL, `* ? [ ] { } \\`, duplicate
separators, `.` or `..` components, or a trailing slash except `/`. No working
directory, shell expansion, case folding or PAP-host filesystem resolution is
used. A path equality does not denote a directory tree or Cedar entity UID.
Deployment uses the existing process-tree Scope; the Translator rejects a
`CgroupId` selector.

Exclusions belong to their target, not to the entire policy. Any other matching
target can still protect an excluded entry. Identical targets with identical
exclusions merge their operation flags. The same path with different target kinds
or exclusion lists is rejected; the Translator does not invent a priority order.
Excluding only a protected directory root while retaining its descendants is
rejected. Exact-file targets never carry exclusions.

The lowering limits are 64 targets, 64 intermediate Boolean branches, 64 literal
set elements, 64 accumulated exclusions per branch and recursive expression depth
64. Exceeding a limit rejects the entire translation; no list is truncated.
An empty effective policy is rejected rather than interpreted as deployment
removal. These limits apply after SDK parsing, not to Cedar parser complexity.

## 3. Rejections and permitted semantic lowering

The following are rejected: permit; unconstrained actions; read/exec/network
actions; unrestricted file.write; create/rename/setattr; `context.input.program`,
`mode`, `ownerUid`, `ownerGid`, `cwd`, `arg_count` and `args` conditions;
resource identity/hierarchy conditions; arbitrary
`like` patterns; unbounded complements; unsupported functions and expressions.
Even an unsupported condition in a constant-false branch is rejected. AgentSight rejects
principal constraints and references, and annotated policies. The shared frontend
checks the schema revision, rejects templates and validates supported annotations.

Only these fixed target semantics are accepted:

| Mapping | Backend boundary |
|---|---|
| delete → deletion protection | Also protects stable entries/ancestors against relevant renames, moves into unprotected ranges, replacement and exchanges. It is not an unlink-only filter or a blanket rename ban. |
| write → ordinary-write protection | Uses Sight's structured `file_permission` / `MAY_WRITE` check. It is not complete content integrity protection; truncate and mmap modification paths are not fully covered by this check. |
| Path ranges → filesystem targets | Sight resolves entries, directory identities and relevant aliases in the selected process's filesystem view. Exact-file protection is not protection of every hardlink to the inode. |
| `static_at_apply` | Mount topology is resolved at apply time. Directory trees cover subsequently created files, but arbitrary later mount changes are outside the guarantee. |

For classification UNKNOWN, the current backend denies deletion/related rename
operations but allows ordinary writes. Ordinary-write-only protection does not
prevent deletion; combine both flags when both protections are required. Relevant
overlay/overlayfs topology is rejected by the backend. Final symlinks cannot be
protected exact-file targets or directory roots; intermediate symlinks are subject
to the backend resolver. The source schema alone does not prove these boundaries.

No other semantic degradation is allowed. In particular, never discard a
condition, unsupported operation, exclusion or statement; never widen a path to
its parent; never enumerate today's glob matches as a persistent rule; never
switch to legacy DSL or audit mode after a structured deployment failure. More
blocking is also a semantic change outside the fixed boundaries above.

## 4. Deletion and write-protection flows

Deletion policy:

```cedar
forbid (principal, action == SecCore::Action::"file.write",
        resource is SecCore::File)
when {
    context.input.operation == SecCore::WriteOperation::"delete"
    && (resource.path == "/workspace" || resource.path like "/workspace/*")
}
unless {
    resource.path == "/workspace/cache"
    || resource.path like "/workspace/cache/*"
};
```

The [deletion source envelope](../../v2/fixtures/adapters/agentsight/cedar-file-protection/prevent-delete.json)
and [write source envelope](../../v2/fixtures/adapters/agentsight/cedar-file-protection/prevent-write.json)
are executable fixtures. The latter changes the operation to `"write"`.
For both protections on the same range use:

```cedar
[SecCore::WriteOperation::"delete", SecCore::WriteOperation::"write"]
    .contains(context.input.operation)
```

The resulting target body for deletion is:

```json
{
  "version": 1,
  "topology_mode": "static_at_apply",
  "targets": [{
    "path": "/workspace",
    "kind": "directory_tree",
    "protections": {"delete": true, "ordinary_write": false},
    "exclusions": [{"path": "/workspace/cache", "kind": "directory_subtree"}]
  }]
}
```

For write-only protection the flags are false/true; for combined protection they
are true/true. Different operation ranges remain different targets.

The shared flow is:

1. CLI/RPC submits the source envelope. PAP validates before saving a Policy
   revision; invalid syntax/schema does not create a revision.
2. Scope pins the Policy revision. Discovery supplies the process identity and
   creates a Binding snapshot; existing assignments do not follow Policy updates.
3. Reconciler invokes Translator. PST conditions become the structured target
   plan, or the whole Binding translation is rejected.
4. Client checks boot ID, PID namespace and process start time and prepares the
   exact HTTP bytes. The existing lifecycle registers the target before mutation.
5. Preflight checks ready/ActPlane, static topology and the required exact-file
   deletion, directory-tree deletion and/or ordinary-write capabilities. Missing
   flags mean unavailable and reject before old-target cleanup or POST. Identity is checked again
   before POST; only a matching enforced receipt with a domain confirms presence.
6. Sight validates target existence/types, topology and capacity, then installs
   protection. Scope deletion uses the saved target reference for cleanup.

With the V2 daemon and Sight configured, commands from `src/agent-sec-core/v2` are:

```sh
agent-sec-cli policy create --name prevent-delete --file fixtures/adapters/agentsight/cedar-file-protection/prevent-delete.json
agent-sec-cli policy create --name prevent-write --file fixtures/adapters/agentsight/cedar-file-protection/prevent-write.json
agent-sec-cli scope create --pid <PID> --policy-id <POLICY_ID> --policy-revision <REVISION>
agent-sec-cli binding list
agent-sec-cli scope delete --scope-id <SCOPE_ID>
```

Use one selected Policy for each Scope; combine both operation constraints into
one Policy when both are needed on a process. Inspect the resulting Binding
status: successful Policy creation only establishes admission. The paths must
exist in the target process's filesystem view before deployment.

Static unsupported input returns a bounded rejection code such as
`CEDAR_RULE_1_UNSUPPORTED_CONDITION`. Within the Translator, input/schema failures
use `INVALID_CEDAR_POLICY`; an empty result uses `CEDAR_EMPTY_EFFECTIVE_POLICY`.
Policy create/update RPC validation failures use `invalid_argument`.
Live capability and target preparation failures remain Client/backend failures.
Client update preflight runs before old-target cleanup, but target-side path or
topology validation occurs on POST: an update can remove old protection before
the new policy fails. This change preserves that existing lifecycle and does not
add transactional replacement or automatic rollback.

## 5. Implementation, validation and rollback

`cedar.rs` checks policy heads and merges targets; `cedar/expressions.rs` lowers
Boolean conditions; `cedar/paths.rs` handles literal ranges and exclusions. They
are private implementation details, not a new persistent AST/IR. The Client
decodes the artifact, gates capabilities and preserves the replay/receipt checks.
Reconciler and SQLite storage formats are unchanged.

From `v2`, run:

```sh
cargo test -p asc-policy-translator-agentsight -p asc-agentsight-client -p asc-pcp --locked
```

Translator tests compare supported path/operation conditions with Cedar decisions
under an explicit permit baseline, and reject unsupported or oversized input.
Client tests check fixed version-1 JSON requests, rejection of unsupported payload
versions and fields, exact-byte retry, capability failure before update cleanup,
malformed artifacts and mismatched acknowledgements.
The PCP test `cedar_delete_and_write_flow_through_pap_sqlite_reconciler_and_http`
exercises both fixture policies through PAP, SQLite, instance admission, actual
Translator/Client/Ureq, HTTP receipt, Binding Ready and cleanup. Discovery and
the remote PEP are controlled test doubles. This does not establish live kernel
enforcement or full file integrity.

Rollback requires revoking structured deployments before returning to a binary
that cannot translate Cedar. Existing native DSL request bytes remain compatible;
Cedar source/snapshot persistence retains the compatibility limits documented in
[the overall design](POLICY_FRONTEND_AST_AND_TRANSLATION.md). No database migration
or policy-source conversion is introduced.

## 6. Native JSON translation boundary

`AgentSightTranslator::translate` reads the complete Binding snapshot. It validates
the whole input, lowers its rules to ActPlane DSL and packages a target plan.
It does not query the latest template, evaluate a request or perform target I/O.

| AST property | Translator accepts | Otherwise |
|---|---|---|
| Effect | `block` | `RULE_<index>_UNSUPPORTED_EFFECT` |
| Category/action | `file` / `write` | `RULE_<index>_UNSUPPORTED_ACTION` |
| Condition | Exactly `{"operation":{"eq":"delete"}}` | `RULE_<index>_UNSUPPORTED_CONDITION` |
| History | Absent | `RULE_<index>_UNSUPPORTED_HISTORY` |
| Pattern | Literal or literal prefix followed by `/**` | Unsupported-glob rejection |
| Size | At most 128 rules; lowered pattern at most 63 bytes | Explicit limit rejection |
| DSL literals | No quotes, backslashes or control characters in path/reason | Explicit pattern/reason rejection |
| Attachment | Process tree rooted at the Binding's pinned instance | cgroup selector rejected |

A valid but unsupported rule rejects the entire Binding translation; no supported
subset is silently sent. Rule order determines stable generated names, not a
general first-match/last-match decision algorithm. Multiple supported block rules
cover the union of their matches. No complex-condition simplification occurs.

This template uses the first rule from the
[prepared-Binding fixture](../../v2/crates/asc-policy-types/tests/fixtures/prepared-binding.json):

```json
{
  "specVersion": "0.1",
  "rules": [
    {
      "effect": "block",
      "category": "file",
      "action": "write",
      "target": { "type": "file", "path": "/etc/agent/config.yaml" },
      "where": { "operation": { "eq": "delete" } }
    }
  ]
}
```

Its DSL is:

```text
source AGENT = exec "**"
rule agentseccore-unlink-0000:
  block unlink file "/etc/agent/config.yaml" if AGENT
  because "AgentSecCore file deletion policy"
```

An explicit `because` replaces the fallback reason. SecCore emits a
`TargetBindingPlan` with format `agentsight.actplane.binding.v2`, containing source
IDs/revisions, the DSL and the pinned process-tree attachment.

Sources: [translator](../../v2/crates/asc-policy-translator-agentsight/src/translate.rs),
[plan](../../v2/crates/asc-policy-translator-agentsight/src/plan.rs),
[translation tests](../../v2/crates/asc-policy-translator-agentsight/tests/translation.rs).

## 7. AgentSight backend deployment details

The native JSON/DSL path uses the following protocol:

1. The Client rechecks process identity and requires health `ready=true`, backend
   `actplane` and `capabilities.file_delete_guard=true`. It sends an authenticated
   `POST /api/enforcement/bindings` with the DSL, policy ID/revision, root PID,
   start time and `policy_mode=enforce`. The default API root is
   `http://127.0.0.1:7396/api`. Product Bindings currently supply no Agent identity;
   `agent_id` is empty and `session_id` is omitted.
2. AgentSight forwards through its Coordinator to the local Enforcer over UDS.
   The Enforcer compiles DSL, seeds the target domain/label and submits compiled
   policy bytes to ActPlane. The Client accepts a matching response only when it
   reports `Enforced` and a domain ID. Conditional repository writes then record
   observations and `READY`; transient failures use bounded retries.

There are two different compilations: SecCore AST → DSL, then Enforcer DSL →
execution configuration. The current ActPlane compiler lowers both `write` and
`unlink` to `OP_WRITE`; generated DSL or a deployment response does not prove
delete-only behavior on a real kernel. Live deny tests remain separate evidence.

Sources: [Client](../../v2/crates/asc-agentsight-client/src/client.rs),
[Enforcer](../../../agentsight/crates/agentsight-enforcer/src/actplane.rs),
[operation lowering](../../../agentsight/crates/actplane-ifc-compiler/src/dsl/lower.rs).
