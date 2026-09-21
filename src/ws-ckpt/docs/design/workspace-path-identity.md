# Workspace path identity and recovery confirmation

[中文版](workspace-path-identity_zh.md)

## Registration invariant

A workspace has one ID and one user-facing registration anchor. Normalize the
parent of that anchor, following filesystem symlink and `..` semantics, without
following the final workspace link. Registration, persisted-state rebuild, lookup,
and conflict detection share this normalization. Missing ordinary parent
components can be retained for detached registrations; dangling parent symlinks
and unresolved `..` are rejected. The root directory and backend-internal paths
are never registration anchors.

Both registry directions are published under one short lock. Duplicate IDs or
normalized anchors fail before either map changes. No registry lock spans an
async operation or workspace lock. Persisted conflicting or internal anchors
fail visibly instead of silently selecting one owner.

An ID or registered anchor takes precedence over the current link target.
Checkpoint, rollback and workspace policy requests require a live registration;
recovery permits detached registrations. Global status remains read-only and can
list detached workspaces; targeted status retains its existing detached error.
Guarded V2 identity retains its stricter exact-registration-path contract.

Re-adoption accepts only a canonical direct child of the backend data root. An
existing index supplies its user anchor, which must still resolve to that live
root. With no index, a user-facing link and the matching snapshot bucket are
required. An internal path without a proven user anchor is refused.

## Recovery protocol

`RecoverPreview { workspace }` resolves the request in the daemon and returns
`RecoverPreviewOk { preview }`. `RecoveryPreview` contains `ws_id` (absent for
orphan backup recovery), `registration_path`, `snapshot_count`, and an opaque
`confirmation_digest`. Snapshot count covers actual directories selected by the
backend for deletion, including unindexed snapshots.

The CLI displays this result and sends `RecoverConfirmed { preview }`. The daemon
addresses the returned ID rather than resolving the original alias again, then
recomputes the preview under the initialization and workspace mutation locks.
A changed target, indexed snapshot set or physical deletion set requires a new
confirmation. Orphan previews bind to the backup and report zero deletions:
that recovery retains migrated storage and snapshots. `--force` still uses the
same preview and revalidation, skipping only the interactive prompt.

The new bincode enum variants are appended; existing request and response tags
are unchanged. Legacy `Recover` remains available to old clients. New CLI builds
require a daemon supporting preview and fail visibly rather than falling back to
an unchecked recovery. Upgrade or roll back the CLI and daemon together.
