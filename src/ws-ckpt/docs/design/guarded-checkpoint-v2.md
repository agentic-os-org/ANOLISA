# Guarded Checkpoint V2

[中文版](guarded-checkpoint-v2_zh.md)

Guarded Checkpoint V2 is an append-only protocol for governed callers. It does
not change the legacy `Checkpoint` behavior. Legacy requests still perform lazy
bootstrap and auto-initialization; the three V2 requests accept only an already
registered workspace identity and never canonicalize, initialize, adopt,
repair, or re-register a caller-supplied path.

## Protocol flow

1. `WorkspaceIdentityV2` performs an exact lookup using the original registered
   path string. It returns the `ws_id`, the daemon's registered path, and an
   opaque generation token. The query also verifies at that point that the
   registered path resolves to `backend.data_root()/ws_id`; it performs no
   backend write.
2. `GuardedCheckpointV2` accepts only canonical `ws-<6hex>[-N]` workspace IDs.
   Under the per-workspace lifecycle lock and workspace write lock, it reads the
   live generation again. A token mismatch or unregistered workspace is
   rejected before the snapshot backend runs.
3. `CheckpointEvidenceV2` queries durable evidence by exact workspace,
   generation, checkpoint ID, operation digest, and Unix peer UID. It does not
   compare the current live generation because historical checkpoint evidence
   must remain queryable after a rollback.

## Generation identity

The daemon reads `BTRFS_IOC_FS_INFO.fsid` and
`BTRFS_IOC_GET_SUBVOL_INFO.uuid` from the same live-subvolume file descriptor,
opened with `O_NOFOLLOW`, and hashes them with the V2 domain separator. The FSID
distinguishes Btrfs filesystems, while the subvolume UUID distinguishes writable
subvolumes created by rollback within one filesystem. An ioctl error, all-zero
identity, non-subvolume root, or non-Linux environment fails closed. There is no
device/inode fallback.

The token identifies the writable-subvolume generation; it does not identify
every ordinary content write inside that subvolume.

The registered path is replaceable by the workspace owner, so it is only the
daemon's saved user-facing representation. V2 performs a point-in-time symlink
mapping check before identity resolution and creation. Empty-workspace checks
and snapshot creation always use the internal `data_root/ws_id`, never the
replaceable registered path. The generation check and backend creation operate
on that internal live subvolume while holding the same daemon locks.

## Guarded creation and evidence

Before the backend runs, guarded creation validates syntax, registration,
generation, ID conflicts, peer credentials, quiescence, and metadata. After a
successful snapshot, the daemon clones the snapshot metadata and evidence into
the next `SnapshotIndex`, strictly persists file contents, rename, and parent
directory, and only then publishes the state in memory. An empty workspace's
`Skipped` evidence follows the same save-before-publish order.

Evidence records the `caller_uid` obtained from the listener's `SO_PEERCRED`.
This is receipt attribution, not a daemon UID allowlist; the socket retains its
existing `0o666` authority model.

A checkpoint ID cannot be reused while its evidence is retained. An exact
guarded duplicate returns the original evidence without invoking the backend.
Legacy `Checkpoint` also refuses to reuse a retained ID, preventing old
evidence from claiming a new subvolume created under the same ID after cleanup.
A governed client with an incomplete write or response must reconcile through
`CheckpointEvidenceV2`, not replay the create request as a result probe.

## Retention and durability

Each workspace retains at most 256 guarded evidence records. At capacity, the
daemon may evict `Skipped` records or `Created` records whose index metadata is
explicitly `missing`. A later query for an evicted record returns Unknown and
its ID may be reused. A successful cleanup or deletion removes evidence only
after the backend confirms deletion of the corresponding snapshot. Records
temporarily detached from the index but not confirmed deleted by the backend
cannot be evicted.

If all 256 records describe `Created` snapshots that may still exist, a new
request is rejected with `EvidenceCapacityReached` before the backend runs.
This bound prevents the world-writable socket from growing the root-owned index
without limit; visible snapshots remain governed by the existing cleanup and
pin policies.

`GuardedCheckpointV2Rejected` means the backend did not run and therefore the
checkpoint had no side effect. An error after backend execution begins uses the
legacy `Response::Error`; the caller must treat it as uncertain and query
evidence. Created is conclusive only when exact evidence exists and the Created
snapshot metadata exists with `missing == false`. Every other result is
Unknown.

C0 has no prepared journal or permanent receipt ledger, so a process crash
between backend success and evidence persistence may leave a permanent Unknown.
A strict parent-directory fsync can also report failure after rename succeeds,
so evidence may differ before and after restart. Guarded saves strictly fsync
the parent directory. Every legacy index writer performs at least temp-file
fsync, rename, and best-effort parent fsync, so later legacy checkpoint,
rollback, cleanup, delete, or orderly shutdown does not rewrite acknowledged
evidence into an unsynchronized file.

## Guarded rollback path representation

Guarded rollback preview and the diff recomputed immediately before rollback
require lossless UTF-8 paths. Btrfs dump paths are unescaped before UTF-8
validation. Invalid path bytes return `UnsupportedWorkspacePath`, with the dump
operation, path field (for example, `rename destination`), and original escaped
token in the daemon diagnostic. Raw non-UTF-8 bytes in ignored metadata fields
from btrfs-progs 6.1 do not fail path parsing.

This rejection occurs before backend rollback. If an operation was already
reserved, the daemon clears that reservation durably before returning the
rejection; failure to clear it retains the existing uncertain-outcome behavior.
Cosh maps the rejection to `CheckpointRestoreFailed`, `recoverable = false`,
and `details.guarded_rollback_rejection = "unsupported_workspace_path"`. It keeps
daemon diagnostic text private; use the ws-ckpt diff diagnostic to locate the
affected path. An unchanged path cannot be fixed by retrying. Ordinary rollback
without preview does not depend on this diff, but it does not provide the guarded
diff-digest check.

`UnsupportedWorkspacePath` is appended at bincode discriminant 16 in both
ws-ckpt and cosh; existing rejection discriminants 0–15 remain unchanged. Deploy
both components together to expose this classification. Older cosh clients
cannot decode the new rejection and fail closed with a non-recoverable protocol
error instead.

Legacy `Diff` and `RollbackPreview` requests preserve the same typed path
failure through the dispatcher as `ErrorCode::UnsupportedWorkspacePath`, appended
at discriminant 13 in both ws-ckpt and cosh's mirrored `WsCkptErrorCode`. Other
unclassified handler errors still use `InternalError`. The CLI prints the
dedicated code with the operation, field, and escaped path token. Legacy cosh
calls map it to `CheckpointRestoreFailed` with `recoverable = false`.

OpenClaw and Hermes recognize `cannot be represented as UTF-8` before generic
error substrings, preserving the diagnostic and stating that the failure is not
retryable until the filename is fixed. This also recognizes diagnostics from an
older daemon that labels the error `InternalError`. Hermes diff/rollback tool
handlers apply the mapping before returning errors to the model. Deploy the
daemon, CLI, and cosh together for the appended wire code; older CLI binaries
cannot decode it. Update both plugins to receive the explicit retry guidance.

## Backend space admission

Btrfs backends refuse a new subvolume when backend data or metadata usage is
at/above the 95% deletion guard, or when usage cannot be read. This covers the
checkpoint snapshot, the rollback snapshot, and the diff temp snapshot used by
rollback preview and revalidation. The refusal happens before any snapshot is
created or the workspace is moved, so guarded checkpoint and rollback return
`InsufficientSpace` instead of an uncertain outcome; a reserved rollback is
cleared durably first. Retry the same request after cleanup reclaims space.
Legacy requests return the existing `ErrorCode::DiskSpaceInsufficient` for the
same refusal instead of `InternalError`.

Cosh maps `InsufficientSpace` to `CheckpointCreateFailed` or
`CheckpointRestoreFailed` with `recoverable = true` and
`details.guarded_rejection` / `details.guarded_rollback_rejection =
"insufficient_space"`, using its own fixed message. The daemon diagnostic stays
private and is also logged by the daemon.

`InsufficientSpace` is appended at bincode discriminant 12 for checkpoint
rejections and 17 for rollback rejections in both ws-ckpt and cosh. Older cosh
clients cannot decode it and fail closed with a non-recoverable protocol error.

## Compatibility and validation

New request and response variants are appended to the bincode enums. All legacy
discriminants and the legacy `SnapshotMeta` wire layout remain unchanged.
`SnapshotIndex.governed_evidence` uses `#[serde(default)]`, so an old JSON index
loads with an empty map. Failure to decode an identity request on an old daemon
means V2 is unsupported; a caller must not fall back to legacy `Checkpoint`,
which can auto-initialize.

This increment supplies only the ws-ckpt daemon protocol and persistence
foundation. It does not integrate the cosh-ng Gateway or Runtime and does not
admit checkpoint provider authority. Unit tests cover the Btrfs ioctl ABI,
fixed hash vectors, and non-Btrfs fail-closed behavior.

Pre-review revision `d9b704d7` was also exercised on a privileged x86_64 Linux
ECS using a private mount namespace and a temporary 1 GiB
device-mapper-backed Btrfs filesystem. The run covered real ioctl identity,
stale-generation rejection before backend execution, guarded creation,
duplicate idempotency, exact evidence, a real writable-subvolume rollback that
changed the generation, rejection of the old generation, evidence
reconciliation after daemon restart, and caller-UID mismatch rejection. All
temporary devices, filesystems, sockets, and runtime state were removed.

Power-loss injection, kill-during-rollback crash recovery, a release build, and
manual Terminal testing remain untested. Offline filesystem copies with the
same FSID and subvolume UUID are outside this protocol's identity threat model.
