# Run Metadata Storage

`RunOutputStore.write_run_metadata` merges batch contributions into
`run_metadata.json` for later trace export. The JSON schema and merge rules
remain unchanged: counts accumulate, instance IDs are deduplicated, and later
serialized contributions replace mappings for the same instance.

## Transaction boundary

A `filelock.FileLock` on `.run_metadata.lock` protects the complete load, merge,
and publication operation. Each writer uses a separate lock object, so separate
store instances, threads, and processes coordinate through the filesystem.
The lock path is stable while the metadata file is atomically replaced. Do not
remove the sidecar while writers are active, as that can split lock ownership.
Different output directories do not share a lock.

Each writer creates its own temporary file exclusively in the output directory,
writes and closes complete JSON, then replaces `run_metadata.json`. Existing
permission bits are preserved; new files use the normal creation permissions
and umask. Unlocked readers see the previous or next complete file. A failed
write or replacement propagates the error, retains the previous metadata, and
cleans up only the temporary file created by that writer. The lock is released
on exit. A forcibly terminated writer can leave a temporary file, which is not
loaded as metadata; do not clean such files while writers are active.

## Limits and compatibility

Coordination applies to cooperating updated writers on a local filesystem with
working file locks and same-directory atomic replacement. Mixing older writers,
removing the sidecar during use, or using a filesystem without those semantics
is outside this guarantee. This is atomic visibility, not a power-loss durability
guarantee; no fsync transaction is introduced.

Predictions, instance summaries, trace naming, and whole-run artifact consistency
are outside this transaction. Independent runs should still use different output
directories. The existing handling of already invalid metadata (warning and a
fresh payload) is unchanged. No schema migration is needed. Reverting the code
restores the old write behavior; the sidecar itself is not result data.
