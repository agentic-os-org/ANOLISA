# Lifecycle State Consistency and Compatibility

[中文版](lifecycle-state-consistency_zh.md)

Blaze has three related lifecycle boundaries. Before serving requests, it must
reconstruct a complete persisted sandbox inventory without exposing a partial
result. While serving requests, it exposes lifecycle and guest operations only
through the sandbox namespace and rejects reserved reusable-capacity operations
before they can change ownership. Checkpoint capture must publish artifacts,
checkpoint history, and lifecycle state in a recoverable order. Retired
`Reset`, `Warm`, and `start_path = "warm"` values remain decodable so startup
can clean non-terminal records that contain them.

This document defines all three boundaries. The inventory-publication protocol
does not change the HTTP API, configuration keys, or persisted JSON format. The
management API section defines the sandbox namespace and the reserved
reusable-capacity boundary. The checkpoint section defines four sandbox routes —
capture, history, pruning, and restore — the durable operation fields used to recover
interrupted capture, and the restore journal and lifecycle contract that keep an
interrupted restore recoverable.

## Terms and owned objects

The **state root** is the directory configured by `daemon.state_dir`. Each
persisted sandbox record is stored in one canonically named UUID directory
below that root. Its `state.json` file contains the lifecycle record used
during restart.

`StateStore` is the supported entry point for lifecycle-record persistence. It
keeps the opened state-root directory object for its lifetime instead of
reopening the configured pathname. For each active sandbox, it also retains an
opened UUID directory object. Later record and runtime-directory operations are
derived from these opened objects.

The **startup inventory** contains the validated lifecycle record for every
UUID-owned directory. A separate retained-owner map keeps the opened UUID
directories that the daemon must continue to use for later lifecycle and
backend operations.

## Writer coordination

A production daemon takes a non-blocking exclusive advisory lock on the opened
state root before it scans lifecycle records. Another Blaze daemon following
the same protocol cannot start with that state root until the first daemon
releases the lock.

Inside one daemon, the startup scan holds the `StateStore` run-directory map
lock for the complete scan and publication sequence. Lifecycle persistence
also enters this map through `StateStore`, so a supported in-process writer
cannot publish or release an owner while startup is constructing the
inventory. Per-sandbox record writes have an additional writer lock.

These two locks serve different purposes: the state-root lock coordinates
cooperating daemon processes, while the run-directory map lock coordinates
writers inside one daemon.

## Startup publication protocol

Startup follows this order:

1. Open the configured state root, take its advisory lock, and retain that
   opened directory object.
2. Enumerate UUID-owned entries and build private instance and retained-owner
   maps. For every UUID entry, require:
   - a canonical lowercase, hyphenated UUID directory name;
   - a directory rather than a link or another filesystem object, and the same
     directory object observed during enumeration;
   - a regular `state.json` with exactly one hard link, opened relative to that
     directory instead of through a replacement path;
   - a record whose sandbox ID matches the directory name; and
   - for `Destroyed`, no active operation and backend ownership of
     `NotStarted` or `Stopped`.
3. Complete a second enumeration of canonical UUID names and compare its full
   set with the first scan.
4. After the second enumeration has finished, revalidate every retained UUID
   directory and `state.json` against the objects accepted by the first scan.
5. Only after every check succeeds, publish the retained-owner map and return
   the instance map to `ServerState`.
6. Reconcile the accepted sandbox records, then bind the configured Unix and
   TCP API listeners.

The name-set comparison must finish before object revalidation begins. This
ordering prevents an early owner from being accepted while the final directory
enumeration is still processing later UUID entries.

## Failure behavior

Any missing, malformed, unexpectedly typed, aliased, or internally
inconsistent UUID record stops daemon startup. If the final name-set comparison
or object revalidation detects an added, removed, or replaced owner or record,
startup also stops. The scan does not publish a partial retained-owner map, and
the daemon does not open its API listeners.

Blaze leaves a rejected UUID directory and its `state.json` unchanged for
operator inspection and repair. Existing cleanup of state-publication staging
entries remains separate from rejected-record handling.

After a complete inventory has been accepted, startup reconciliation processes
each non-terminal sandbox independently. A cleanup failure for one sandbox can
retain that sandbox in memory as `RecoveryRequired` without turning the already
validated inventory into a partial one. Blaze attempts to persist the recovery
state; if that write also fails, reconciliation reports the additional error
and the durable record may still contain its previous state.

## Checkpoint lifecycle and recovery

`POST /v1/sandboxes/{id}/checkpoint` captures a running sandbox, and
`GET /v1/sandboxes/{id}/checkpoints` lists its committed history. Both
operations hold the same per-sandbox operation lock used by lifecycle and
guest requests. Capture requires a `Running` record with no unfinished
operation, a live matching backend owner, and explicit capture support from
both the backend and storage provider. An unsupported combination returns
`501 Not Implemented` before the backend is paused or lifecycle state changes.

Capture uses this durable order:

1. Validate the current checkpoint parent and create a private staging
   directory.
2. Persist checkpoint intent, including the generated checkpoint ID, before
   pausing the backend.
3. Pause the backend, record that durable phase, capture backend state and the
   provider-owned writable root, then publish an integrity-checked manifest by
   a no-replace rename.
4. Persist publication, atomically move the sandbox checkpoint HEAD, and
   persist the HEAD-update phase.
5. Resume and revalidate the backend, pass through `Checkpointed` back to
   `Running`, record `last_checkpoint`, clear the operation, and persist the
   final lifecycle record.

A failure known to precede publication removes the private stage, resumes the
backend, and clears the operation. If publication, HEAD movement, lifecycle
persistence, or backend resume has an unknown or unsafe outcome, Blaze retains
the durable operation and marks the sandbox `RecoveryRequired`. Startup does
not restore a checkpoint or adopt an interrupted backend; normal reconciliation
cleans the owned runtime and checkpoint transaction artifacts. Committed
checkpoint history is retained until explicit pruning or sandbox destruction.

`POST /v1/sandboxes/{id}/checkpoints/prune` removes committed branches that
are unreachable from the current HEAD. It has no request-body fields and
accepts only a `Running` sandbox with no unfinished operation. The complete
current HEAD lineage is derived before any mutation and is never selected for
removal. For compatibility with Go Blaze, the request body is not inspected;
the body stream is not polled, and an absent body, `{}`, obsolete fields, or
non-JSON content does not change which checkpoints are retained or removed.

Prune persists an `OperationKind::Prune` record before changing the catalog.
Each candidate is revalidated and atomically renamed, without replacement, to
a uniquely named prune tombstone before recursive removal. This supports the
version-2 layout, whose backend-owned subtree may contain nested directories.
Successful completion removes the tombstone and clears the operation record.

A failure proven to precede the first rename clears the operation record and
leaves committed history unchanged. After any earlier removal, the same
failure is treated as partial completion. A failure after a rename, or an
outcome whose namespace state cannot be proved, retains the operation and
marks the sandbox `RecoveryRequired`. Prune cannot then be retried. Destroy or
startup reconciliation removes the runtime and complete checkpoint namespace,
including any recognised prune tombstone. A daemon interruption after prune
intent is durable follows this same cleanup path instead of resuming deletion.
Operators must destroy the affected sandbox or allow startup reconciliation to
clean it after a daemon restart; checkpoint identifiers in the error text are
diagnostic context, not an authoritative deletion inventory.
An entry that starts like a prune tombstone but fails the strict name check is
not resumed as prune work. Its presence indicates an unexpected catalog entry;
operators should destroy the affected sandbox instead of renaming the entry by
hand. Destroy removes the complete owned checkpoint namespace, including that
entry.

An unreadable or invalid checkpoint catalog is not treated as empty history;
neither is a non-empty catalog whose HEAD file is missing. Before prune loads
the catalog, Blaze enumerates the complete top-level namespace and accepts only
the optional HEAD file and canonically named committed checkpoint directories.
Unknown files, directories, staging entries, or cleanup remnants therefore stop
prune before deletion. Before selecting candidates, Blaze verifies the exact
file inventory, recorded size, and SHA-256 digest of every committed checkpoint.
It also validates parent existence and cycle freedom across every branch,
including branches outside the HEAD lineage. If any namespace, catalog,
ancestry, or artifact-integrity check fails before the first rename, prune
returns HTTP 500, clears its operation record, and leaves HEAD and every
checkpoint directory unchanged. This preflight reads every stored artifact, so
prune time and storage input/output grow with the total checkpoint history. A
validation error indicates storage corruption and should be investigated
instead of retried as an empty prune.

`POST /v1/sandboxes/{id}/rollback/{checkpoint_id}` replaces a running sandbox
from one verified full checkpoint. Before mutation, Blaze verifies the complete
checkpoint ancestry and artifacts, matches the policy, image, backend, version,
and snapshot kind, and requires explicit backend and storage restore
capabilities. Unsupported combinations return `501 Not Implemented` while the
current backend and lifecycle record remain unchanged.

Restore uses this durable order:

1. Persist restore intent and stage an independent root filesystem while the
   current backend remains owned and running.
2. Stop the current backend, record `RestoreBackendStopped`, and enter
   `Restoring` only after that boundary is durable.
3. Activate the staged root while retaining its predecessor, prepare backend
   ownership, and start and validate the replacement owner.
4. Move checkpoint HEAD, commit replacement storage, return the lifecycle to
   `Running`, and clear the restore journal.

A failure before Blaze begins stopping the backend aborts staged storage and
preserves the running backend. Once Blaze starts stopping it, any later failure —
including the stop itself failing or shutdown not being confirmed — retains the
backend and storage ownership that can still be proven and commits
`RecoveryRequired`; destruction uses that journal to complete cleanup. Restore
changes catalog HEAD but does not rewrite the most recently completed capture
recorded by `last_checkpoint`.

## Management API and reusable-state boundary

Lifecycle and guest operations are registered under `/v1/sandboxes`.
Action-style reset and destroy paths are unregistered and return
`404 Not Found`. Canonical destruction remains
`DELETE /v1/sandboxes/{id}`. Checkpoint capture, listing, pruning, and restore
use the four routes defined in the preceding section.

The following reserved management routes also return `501 Not Implemented` and
do not manage reusable capacity:

- `GET /v1/pools`;
- `GET /v1/pools/{backend}/{class}`;
- `POST /v1/pools/{backend}/{class}/drain`; and
- `PUT /v1/pools/{backend}/{class}/sizing`.

`GET /v1/health` retains its `storage_pool` object for response compatibility;
the file provider reports zero ready, capacity, pending, and quarantined slots.
The metrics endpoint does not publish a reset counter because no reset route is
registered. Pool-hit and pool-miss counters also remain absent because reusable
capacity has no supported success path.

New sandbox creation always records `start_path = "cold"`. Lifecycle
transitions cannot enter `Reset` or `Warm`, so no supported path can produce or
reactivate a reusable sandbox. Blaze retains decoding of legacy `Reset`, `Warm`, and
`start_path = "warm"` values only so startup can release resources owned by
records written by earlier releases. After the complete inventory passes
validation, reconciliation destroys each such non-terminal record. Successful
cleanup reaches `Destroyed`. Failed cleanup retains the in-memory record as
`RecoveryRequired` and attempts to persist that state; a persistence failure is
reported and may leave the prior durable state intact. Reconciliation continues
with other accepted records. Create requests never select or reactivate a
legacy record.

## Consistency boundary

This protocol covers lifecycle-state writers that use `StateStore` and daemon
processes that participate in the state-root advisory lock. The advisory lock
does not prevent an unrelated process from modifying the directory directly.
A finite sequence of directory scans cannot provide an atomic snapshot against
such a writer.

Direct modification that bypasses the state-root lock is unsupported. Stronger
isolation for that path is tracked in
[#2459](https://github.com/alibaba/anolisa/issues/2459).

## Maintainer invariants

Future lifecycle-state changes must preserve these rules:

- production lifecycle writes go through `StateStore`;
- the state-root owner is acquired before inventory scanning and retained
  while request handlers can write lifecycle state;
- startup holds the run-directory map lock until the complete inventory is
  accepted or rejected;
- the final UUID enumeration completes before retained objects are
  revalidated;
- no request handler can observe either startup map before all inventory
  checks have passed;
- unregistered sandbox action routes return `404` before reading or changing
  sandbox state;
- checkpoint capture keeps the per-sandbox operation lock until every
  supervised backend, storage, publication, and state task has converged;
- a checkpoint is never exposed as committed history before its artifacts and
  manifest are durably published, and HEAD never names an unpublished entry;
- checkpoint pruning records its operation before mutation, never selects a
  HEAD-reachable lineage, and treats every uncertain rename or partial cleanup
  as recovery-required;
- pool-management rejections occur before lifecycle, runtime, or storage
  ownership changes; and
- lifecycle operations cannot enter or reactivate `Reset` or `Warm`; legacy
  values are cleanup inputs only.
