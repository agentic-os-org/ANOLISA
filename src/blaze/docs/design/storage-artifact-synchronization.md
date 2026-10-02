# Storage Artifact Synchronization

[中文版](storage-artifact-synchronization_zh.md)

Blaze can periodically ask the configured `StorageProvider` to persist the
already-written files and directory metadata owned by running sandboxes. This
closes the gap between a provider that can synchronize one slot's host
artifacts and a daemon that schedules the operation safely across all eligible
sandboxes.

Periodic synchronization is disabled by default. Set
`storage.sync_interval` to a positive duration to enable it.
`storage.sync_timeout` is a positive duration that bounds how long the
scheduler waits for each provider attempt and defaults to 30 seconds.

## Which sandboxes are synchronized

At the beginning of a sweep, the manager selects records whose lifecycle state
is `Running`. Before it calls the provider for one sandbox, it tries to enter
the same operation lock used by lifecycle changes and guest exec, read, and
write requests without waiting. A busy lock defers that sandbox to a later
sweep so one active sandbox operation cannot prevent the worker from reaching
other eligible records. After acquiring the lock, the manager checks the
record again.

The provider call runs only when all of these conditions still hold:

- the lifecycle state is `Running`;
- there is no unfinished lifecycle operation;
- metadata says the backend is running and the daemon still owns that backend;
- the provider can reconstruct a complete slot from the sandbox ID.

A sandbox that is no longer Running is skipped. An inconsistent Running record
is reported as a failed item instead of being silently omitted. The remaining
sandboxes in the sweep still run.

The first sweep starts after one complete interval. Missed ticks are skipped
instead of queued, so a slow sweep cannot create an unbounded backlog.

## Failure and retry behavior

Each provider attempt has one scheduler deadline covering slot reconstruction
and synchronization. A completed failure leaves the slot owned by the sandbox
and does not change lifecycle state. A later sweep or destroy can therefore
retry the provider operation.

Some filesystem operations cannot be cancelled after they start. When one of
those operations exceeds the deadline, the scheduler reports the timeout but
the attempt continues to hold the sandbox operation lock. It also retains the
single synchronization permit, so later attempts are deferred instead of
creating more blocking filesystem work. Once the late attempt completes, the
lock and permit are released and normal retry resumes. Guest and lifecycle
operations that arrive while the lock is retained wait for the provider work
to finish. The configured timeout bounds scheduler waiting, not their wait.

`StorageProvider::sync_artifacts` is the provider-specific persistence boundary.
The file provider calls `sync_all` for the canonical `rootfs.ext4`, `mem.bin`,
`mem.diff`, and `rootfs.diff` files, then synchronizes the slot directory.
Other providers can use a different mechanism while preserving the same
ownership-until-completion contract.

## Checkpoint artifact capture and publication

Checkpoint capture is a separate, explicit provider capability. The default
`StorageProvider` implementation reports no support and returns an error, so a
provider cannot silently opt into partial capture. The file provider
reconstructs the canonical slot from the sandbox ID, retains the opened source
file, and copies the writable root into a private target owned by the
checkpoint transaction. It preserves sparse extents when possible and never
replaces an existing target.

The checkpoint catalog is derived from the retained state-root directory. Each
sandbox has private staging entries, committed checkpoint directories, and one
HEAD reference. A checkpoint carries two producer-owned payload subtrees:
`backend/`, whose internal layout is private to the backend adapter, and
`storage/`, where the storage provider captures the writable root as
`rootfs.snap`. Publication walks both subtrees, refuses symbolic links and
non-regular files, requires the writable-root capture to be present, records
every file's relative path, size, and SHA-256 digest as the manifest
inventory, synchronizes the files and directories, and publishes the
checkpoint with a no-replace rename. HEAD is updated atomically only after
that publication is durable. Listing reopens committed manifests, and
verification requires the directory contents and the manifest inventory to
account for each other exactly before reporting history and HEAD
reachability.

Blocking file copies, manifest publication, and HEAD updates remain supervised
after request cancellation and retain the sandbox operation lock until their
outcome is known. A known pre-publication failure removes only the private
stage. An uncertain publication never removes a path whose identity cannot be
proven. Sandbox destruction removes transaction artifacts and committed
checkpoint history under the same state-root ownership boundary. Checkpoint
deletion and pruning are not part of this protocol.

Checkpoint restore is a separate opt-in provider capability. The file provider
copies the verified checkpoint root into a private stage while the live root
remains selected. Activation atomically selects the stage and retains the
predecessor. Abort restores the predecessor; commit durably records its intent,
then removes the predecessor and transaction journal. Every transition is
idempotently reconciled from the journal after restart. Unexpected links,
replacement paths, ambiguous layouts, or an unverified transaction identity
fail closed without deleting an object whose ownership cannot be proven.

## Capability boundary

Each provider synchronization call persists the already-written artifact bytes
and directory metadata visible to that call. Changes that race with an attempt
may reach the persistence boundary in that attempt or a later one.

## Daemon shutdown

The daemon supervises the periodic worker while serving requests. If the
worker exits unexpectedly, the daemon leaves its accept loop and reports the
worker failure. On a termination signal, the daemon first leaves that accept
loop, then cancels and joins the periodic scheduler. Provider work that cannot
be cancelled remains under its sandbox lock until it completes.

This change owns only the worker lifecycle. Draining already accepted
connections and releasing daemon-wide runtime resources are separate shutdown
responsibilities. Until those responsibilities are implemented, operators
must not treat service-loop return as proof that every in-flight handler and
runtime owner has finished.
