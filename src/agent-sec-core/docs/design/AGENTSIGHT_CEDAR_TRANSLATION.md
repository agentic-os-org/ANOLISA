# AgentSight Adapter: Cedar file protection

[中文版](AGENTSIGHT_CEDAR_TRANSLATION_zh.md)

This document defines the AgentSight Translator's supported Cedar subset and
file-protection target format. It extends Cedar admission and persistence at
`af1bc18a9`; native JSON policies retain their existing DSL translation.
The Translator produces structured plans. Deployment Client support for this
artifact is pending; the current Client rejects its media type.

## 1. Contracts and versions

| Boundary | Contract |
|---|---|
| Cedar source | `seccore.system`, revision `1`; original source stays in Policy and Scope/Binding snapshots |
| Translator input | `PreparedBinding`; Cedar source is checked and converted with `asc_policy_cedar::pst` |
| Target plan | `agentsight.actplane.binding.v2`, `schemaVersion: 2` |
| Structured artifact | `application/vnd.agentsight.file-protection.v1+json`; `content` is serialized JSON text |

The v2 target plan and version-1 file-protection payload are separate contracts.
This change does not introduce native authoring `specVersion: "0.2"` or change
SQLite's version. SecCore's `asc-agentsight-protocol` defines the version-1
payload types, version and media type for the Translator and subsequent Client
integration. It depends only on `serde`; a fixed JSON fixture checks the wire
contract. New payload versions require explicit support and compatibility
validation against the target release.

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
Deployment uses the existing process-tree Scope; the Adapter still rejects a
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
Even an unsupported condition in a constant-false branch is rejected. The common
frontend separately rejects unknown schema revisions, principal constraints and
references, templates, and annotations including `@id` and `@advice`.

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

## 4. Deletion and write-protection examples

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

Unsupported translation input returns a rejection code such as
`CEDAR_RULE_1_UNSUPPORTED_CONDITION`. The Translator reports invalid Cedar as
`INVALID_CEDAR_POLICY` and an empty result as `CEDAR_EMPTY_EFFECTIVE_POLICY`.

## 5. Implementation and validation

`cedar.rs` checks policy heads and merges targets; `cedar/expressions.rs` lowers
Boolean conditions; `cedar/paths.rs` handles literal ranges and exclusions.
These are private translation helpers. Reconciler and SQLite formats are unchanged.

From `v2`, run:

```sh
cargo test -p asc-policy-adapter-agentsight --locked
```

Tests compare supported path/operation conditions with Cedar decisions under an
explicit permit baseline, and reject unsupported or oversized input. These tests
validate translation; deployment and live kernel enforcement require the Client
integration and separate validation.
