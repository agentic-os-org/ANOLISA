# Blaze Firecracker Networking

[中文版](../../zh/runtime/blaze.md)

Blaze can give each Firecracker sandbox a dedicated network namespace, tap
device, veth pair, and address slot. This capability is opt-in and is disabled
by default.

## Prerequisites

The Blaze daemon must run on Linux with permission to manage host networking.
The `ip`, `sysctl`, and `iptables` commands must be installed and executable.
Firecracker and its kernel and root filesystem images must also be available.

Blaze checks these prerequisites when a loaded policy both enables networking
and selects Firecracker as an eligible backend. Policies that leave networking
disabled do not require these host capabilities.

## Configuration

Set `enable_network` in the Firecracker section of a workload policy:

```toml
[select]
backend_priority = ["firecracker"]

[backend.firecracker]
enable_network = true
```

The option applies only to Firecracker. Its default is `false`, so existing
policies retain their previous behavior until they opt in.

## Runtime Behavior

When a request selects a network-enabled Firecracker policy, sandbox creation:

1. allocates a host-wide network slot;
2. creates an owner-qualified network namespace;
3. creates the tap and veth devices and configures addresses, forwarding, and
   namespace-local NAT; and
4. starts Firecracker with the tap device attached.

Allocation and deletion use `/run/lock/blaze-network.lock`, which prevents two
Blaze daemon processes on the same host from choosing the same slot at the same
time. Blaze records the namespace owner before creating dependent devices so a
partially completed setup remains attributable to the sandbox.

Explicit sandbox destruction removes the owned namespace and devices after the
backend process has stopped. A compensated startup failure performs the same
cleanup. If cleanup cannot be confirmed, Blaze retains ownership and does not
return the slot to the allocator, allowing a later destroy attempt to retry the
operation.

After a daemon restart, a later destroy request can reconstruct a recorded
network slot. Blaze does not run a background scan or retry controller for
orphaned network resources.

Before any startup reconciliation begins, Blaze loads the complete persisted
sandbox lifecycle inventory. Every UUID owner directory and its `state.json`
must be canonical and directly readable. Before accepting the inventory, Blaze
completes a second enumeration of the canonical UUID names and compares the
complete set with the initial scan. It then revalidates every retained owner
directory and `state.json` against the objects read earlier. Startup stops
before the API listeners open if the second enumeration finds a missing or new
UUID, if the following object checks find a replacement, or if a `Destroyed`
record does not prove cleanup completed. Blaze does not repair or delete the
invalid owner directory or its `state.json`; it remains available for operator
repair.

Blaze state writes are supported only through `StateStore`. A production daemon
keeps an exclusive advisory lock on the state root, and the startup scan holds
the in-process ownership-map lock until publication. These locks serialize
cooperating Blaze writers. A process that modifies the state files directly
without participating in the state-root lock is outside this consistency
contract.

### Detecting an inventory validation failure

Inventory validation happens before Blaze binds either its Unix or optional TCP
listener. If validation fails, the daemon exits with a non-zero status and no
API endpoint is available; `/v1/health` therefore fails to connect instead of
returning a degraded health response.

For the packaged systemd service, inspect `systemctl status blazed` and
`journalctl -u blazed` for the validation error; record-specific errors include
the affected sandbox ID. Blaze leaves the rejected record in place. Repair or
restore that record, then restart the service and confirm that `/v1/health`
responds.

## Sandbox API

Blaze exposes sandbox lifecycle and guest operations under `/v1/sandboxes`.
Clients use this namespace to list, create, inspect, and delete sandboxes and
to execute commands, read files, and write files inside them. Sandbox
destruction uses `DELETE /v1/sandboxes/{id}`. Checkpoint capture and history
use
`POST /v1/sandboxes/{id}/checkpoint` and
`GET /v1/sandboxes/{id}/checkpoints`; unreachable branches are removed through
`POST /v1/sandboxes/{id}/checkpoints/prune`. Restore uses
`POST /v1/sandboxes/{id}/rollback/{checkpoint_id}`. Hibernation uses
`POST /v1/sandboxes/{id}/hibernate` and `POST /v1/sandboxes/{id}/resume`.

## Host Integration Boundary

Blaze configures the sandbox-local network path. Routing beyond the host and DNS
configuration remain the host operator's responsibility. Before enabling the
option in production, configure the required upstream routing or translation
and verify guest connectivity for the host environment.

To disable the capability, set `enable_network = false` or remove the key, then
destroy existing network-enabled sandboxes through the sandbox API.

## Guest Operations

Guest operations are available only while a sandbox is `Running` and its
backend reports a compatible guest endpoint. A cold create that reports such
an endpoint waits for the guest agent before publishing `Running`. Backends
without an endpoint, including production mock fallback, skip that wait and
return HTTP 409 for guest operations.

Guest operations and lifecycle changes use the same per-sandbox operation
lock. After obtaining the lock, the manager checks `Running` again so a request
does not contact an old runtime after a concurrent lifecycle change.

The sandbox routes are:

- `POST /v1/sandboxes/{id}/exec` — execute one command;
- `POST /v1/sandboxes/{id}/read` — read one file; and
- `POST /v1/sandboxes/{id}/write` — replace one file.

Exec requests use the following shape:

```json
{"cmd":"uname -a","cwd":"/","env":{"LANG":"C"},"timeout":10}
```

Write requests provide a path and standard-base64 data:

```json
{"path":"/tmp/input","data_b64":"aGVsbG8="}
```

Read requests provide only `path`. Successful file reads and command output
use standard base64. Exec timeouts range from 1 through 20 seconds. Guest
routes reject an HTTP envelope larger than 22 MiB while reading it, and file
data is limited to 16 MiB after decoding.

A failure before exec or write delivery is safe for caller-directed retry. A
pre-delivery timeout uses `"code": "guest_timeout"`. If delivery began but
the daemon cannot determine the result, it returns HTTP 504 with
`"code": "guest_outcome_unknown"`; reconcile guest state instead of
automatically replaying the operation. Reads do not change guest state.
Oversized input returns HTTP 413. An oversized read response returns HTTP 502
with `"code": "guest_response_too_large"`.

Each request is fully buffered within its per-request limit. The limit does not
bound aggregate concurrency, so clients should also cap concurrent guest
operations. Streaming files, interactive terminals, and session reuse are not
supported.

The optional TCP listener does not yet enforce a daemon-wide access boundary.
Leave `listen.http_addr` disabled in production until
[issue #2223](https://github.com/alibaba/anolisa/issues/2223) is resolved.
Daemon shutdown also does not yet wait for every active HTTP handler or release
all runtime owners, so an in-flight request may observe a closed connection.

## Reusable-Instance Management

The four `/v1/pools` management routes also return HTTP 501. Blaze rejects
`storage.pool_size`, `storage.prefork`, and every `[pool]` section except the
exact historical package defaults. During an upgrade, it temporarily accepts
and ignores only those defaults from the older daemon configuration and two
default policy files, and logs a warning. This exception prevents an
administrator-modified file retained by RPM `%config(noreplace)` from blocking
the new daemon. It does not enable reusable instances. Merge each `.rpmnew`
file or remove the legacy section; later releases may remove this exception.
Any other policy `[pool]` section fails policy loading. At startup,
`policy.on_load_error = "fail"` stops the daemon, while `"warn"` starts with an
empty policy set. A failed administrative or signal-driven reload keeps the
currently active policies unchanged.

The accepted daemon section is exactly:

```toml
[pool]
default_warm_ttl = "30m"
gc_interval = "5m"
```

An accepted policy section must contain exactly these six fields and belong to
one of the two packaged policy identities:

| Policy name | Workload class | `min` | `target` | `max` |
|---|---|---:|---:|---:|
| `agent-rl-default` | `agent-rl` | 4 | 16 | 64 |
| `agent-tool-default` | `agent-tool` | 2 | 8 | 32 |

Both rows require `enabled = true`, `warm_ttl = "30m"`, and
`reset_mode = "full-recreate"`. A missing or additional field, a changed value
or type, a different policy name or workload class, or any other `[pool]`
section is rejected. Accepted compatibility values are ignored and omitted
when configuration is serialized.

Blaze continues to decode persisted `Reset`, `Warm`, and
`start_path = "warm"` values written by earlier releases. Startup
reconciliation treats non-terminal records containing those values as cleanup
candidates and never reuses them. A failed cleanup retains the in-memory record
as `RecoveryRequired` and attempts to persist that state. If persistence also
fails, the startup warning includes the additional error and the durable record
may still contain its previous state. Reconciliation continues with other
accepted records.
The metrics endpoint no longer publishes `blaze_instances_resets_total`,
`blaze_pool_hits_total`, or `blaze_pool_misses_total`.

The lifecycle invariants behind these compatibility responses are recorded in
the
[lifecycle state consistency and compatibility design](../../../../src/blaze/docs/design/lifecycle-state-consistency.md).

## Checkpoint Capture, History, and Restore

Blaze captures a running sandbox through
`POST /v1/sandboxes/{id}/checkpoint`.

Capture requires both the selected backend and the storage provider to
advertise full-checkpoint support. The built-in file provider captures the
writable root filesystem. Firecracker captures guest memory and device state
through its own snapshot API, and the built-in mock backend supplies a complete
development implementation. Bubblewrap and the other process backends do not
advertise capture support in this release. An unsupported combination returns
HTTP 501 before the sandbox is paused or its lifecycle record is changed.

A Firecracker checkpoint records the exact version of the running virtual machine
monitor, because a snapshot can only be loaded back by that same version. Keep
the recorded version installed for as long as you intend to restore from a
checkpoint: upgrading the Firecracker binary does not invalidate stored
checkpoints, but it does mean they can no longer be restored until that version is
available again.

Because that recorded version is what makes a checkpoint restorable at all, capture
refuses a Firecracker sandbox whose monitor does not report one, before the sandbox
is paused. A stored checkpoint without a recorded version therefore cannot be
produced, and the same shape is rejected if it appears in a manifest that is read
back.

For a supported running sandbox, Blaze holds the sandbox operation lock,
validates its current checkpoint parent, quiesces the backend, and captures
the payload as two producer-owned subtrees: the backend adapter writes its
own layout under `backend/` (a VM backend saves its VM state and guest
memory there), and the storage provider captures the writable root
filesystem as `storage/rootfs.snap`. Blaze inventories every captured file,
synchronizes and hashes it, publishes the manifest, atomically updates the
sandbox checkpoint HEAD, and returns the workload to execution. Guest
operations and other lifecycle changes wait for the same operation lock while
capture is in progress.

A successful response contains the complete published manifest. The existing
`checkpoint_id` and `instance_id` fields identify the same checkpoint and
sandbox as `id` and `sandbox_id`:

```json
{
  "checkpoint_id": "ckpt-11111111-1111-4111-8111-111111111111",
  "instance_id": "22222222-2222-4222-8222-222222222222",
  "format_version": 2,
  "id": "ckpt-11111111-1111-4111-8111-111111111111",
  "parent": null,
  "sandbox_id": "22222222-2222-4222-8222-222222222222",
  "policy_name": "agent-tool",
  "image_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
  "backend": "mock",
  "backend_version": "mock-v1",
  "created_at": "2026-08-14T00:00:00Z",
  "snapshot_kind": "full",
  "artifacts": [
    {
      "name": "backend/memory.snap",
      "size_bytes": 8192,
      "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    },
    {
      "name": "backend/vmstate.snap",
      "size_bytes": 4096,
      "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    },
    {
      "name": "storage/rootfs.snap",
      "size_bytes": 8589934592,
      "sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
    }
  ]
}
```

The `artifacts` inventory lists every captured file under its slash-separated
path relative to the checkpoint, sorted lexicographically. Its exact contents
belong to the backend that produced the checkpoint: the example above shows
the built-in mock backend, while a container-shaped backend may record a whole
image directory. Checkpoints written before this format (`format_version: 1`)
remain restorable, but new captures always publish version 2.

Use `GET /v1/sandboxes/{id}/checkpoints` to list committed history. Each list
entry contains `id`, `parent`, `created_at`, total logical `size_bytes`,
`is_head`, and `on_head_chain`. The list is a summary and does not repeat the
complete artifact manifest returned by capture.

A failure known to occur before publication removes its temporary data,
resumes the backend, and leaves the sandbox running. If Blaze cannot prove the
publication, HEAD update, persistence, or backend-resume outcome, it retains
the durable record and reports `RecoveryRequired`; do not retry capture until
the sandbox has been reconciled or destroyed. A committed checkpoint that did
not become HEAD can still appear in history with `is_head: false`.

Remove checkpoint branches that are no longer reachable with:

```http
POST /v1/sandboxes/{id}/checkpoints/prune
```

The request has no defined fields. For compatibility with Go Blaze, the server
does not read or inspect the request body. Clients may omit it, send `{}`, or
send other content, including non-JSON bytes; all supplied content is ignored.
Blaze retains the current HEAD and its complete parent chain, and removes every
other committed branch.

A successful response identifies exactly what left committed history:

```json
{
  "status": "pruned",
  "removed_count": 1,
  "removed": ["ckpt-44444444-4444-4444-8444-444444444444"]
}
```

The route defines no request-body fields and ignores supplied content. A body
therefore cannot select checkpoints or protect an unreachable branch. Prune
accepts only a `Running` sandbox with no unfinished operation; a hibernated,
recovering, or otherwise unavailable sandbox returns HTTP 409.

Capture, list, prune, guest operations, and lifecycle changes use the same
per-sandbox operation lock. Before deleting anything, Blaze persists a prune
operation record. Each selected checkpoint then moves atomically to a unique
`.prune.<checkpoint>.<uuid>.tombstone` directory and is recursively removed,
including nested backend payloads. The current HEAD chain is never a candidate;
the candidate identity and current HEAD are revalidated before every rename.

If cleanup fails before any rename, Blaze clears the operation record and no
checkpoint is removed. If cleanup is partial or the rename result cannot be
proved, the sandbox becomes `RecoveryRequired` and the operation record is
retained. A later prune request in that state returns HTTP 409 without changing
the catalog. Destroy, or normal reconciliation after a daemon restart, removes
the owned runtime and complete checkpoint namespace. Operators should destroy
the affected sandbox, or allow normal startup reconciliation to clean it after
a daemon restart. They must not retry prune or infer an authoritative deletion
set from checkpoint identifiers embedded in the error text. Blaze returns HTTP
200 only after every tombstone created by the request is removed and the
checkpoint namespace is synchronized.

An unreadable or invalid checkpoint catalog is not treated as empty history;
neither is a non-empty catalog whose HEAD file is missing. Before prune loads
the catalog, Blaze checks the complete top-level namespace: only the optional
HEAD file and canonically named committed checkpoint directories are accepted.
Unknown files, directories, staging entries, or cleanup remnants therefore
stop prune before deletion. Before selecting candidates, Blaze verifies the
exact file inventory, recorded size, and SHA-256 digest of every committed
checkpoint, and verifies that every branch has existing parents and contains
no cycle. If any namespace, catalog, ancestry, or artifact-integrity check
fails before the first rename, prune returns HTTP 500, clears its operation
record, and leaves HEAD and every checkpoint directory unchanged. This
preflight reads every stored artifact, so prune time and storage input/output
grow with the total checkpoint history.
Diagnose storage corruption instead of repeatedly calling prune.

Restore a running sandbox with:

```http
POST /v1/sandboxes/{id}/rollback/{checkpoint_id}
```

Restore requires a verified full checkpoint, an exact match for the sandbox's
policy, image, backend, and backend version, plus explicit restore support from
both the backend adapter and storage provider. The Firecracker adapter, the
built-in mock adapter, and the file provider implement this contract. Other
backend adapters return HTTP 501 before stopping the current runtime until they
implement restore.

Restoring a Firecracker sandbox replaces its virtual machine monitor with a new
process that loads the captured memory and device state, so the process
identifier changes while the sandbox identifier does not. The replacement is
started with the same host shape the checkpoint was taken with — its network slot,
guest transport, and console recording — because the snapshot refers to those
devices by name. Console output and monitor diagnostics recorded before the
restore are kept rather than overwritten.

A restore is refused before the running sandbox is stopped whenever the installed
Firecracker version does not match the one the checkpoint recorded, so a version
mismatch costs you nothing.

A `checkpoint_id` that is not in canonical form is rejected with HTTP 400, and a
canonical identifier that names no committed checkpoint is reported as HTTP 404.
Both answers are final: neither changes the running sandbox, so retrying the
same selection cannot succeed.

The file provider stages the selected root filesystem while the current
backend remains running. Blaze then stops the old backend, activates the staged
root, starts and checks the replacement owner, moves checkpoint HEAD, and
commits storage. The dividing line is whether Blaze has begun stopping the old
backend: a failure before that point, while still validating and staging the
replacement root, preserves the running sandbox untouched. Once Blaze starts
stopping the old backend, any later failure — including the stop itself failing
or Blaze being unable to confirm the old backend actually stopped — retains the
resources that actually exist and marks the sandbox `RecoveryRequired` so
destruction can finish cleanup. Restore moves checkpoint HEAD but does not
rewrite `last_checkpoint` or capture history.

## Hibernation and Resume

Hibernation exists to free the host resources a running sandbox holds — the
backend process and its memory — during a period when the sandbox is not needed,
without discarding guest-visible state. Unlike destroy, the sandbox keeps its
identity and storage; unlike checkpoint capture, the live backend does not keep
running afterwards.

```http
POST /v1/sandboxes/{id}/hibernate
POST /v1/sandboxes/{id}/resume
```

Hibernation requires a running sandbox whose backend supports full snapshot
capture, and whose configured adapter can restore the same backend version.
Blaze verifies this before it changes the lifecycle journal, so an unsupported
combination returns HTTP 501 with the sandbox still running. Requests against a
sandbox that is not in the expected state return HTTP 409. Bringing the workload
to a consistent stop is delegated to the backend's quiesce-for-capture hook,
whose default pauses the backend; a self-freezing backend overrides that hook
and does not need separate pause support.

A successful hibernate records intent, quiesces the backend for capture, writes
the backend payload and guest memory into a private staging directory, flushes
the retained storage slot, records each artifact's size and SHA-256 digest in a
manifest, synchronizes the complete image, and only then publishes it and
commits `Hibernated`. The published image is resolved through the retained
sandbox directory descriptor, so a replaced or symlinked instance directory
cannot redirect it.

Resume verifies the manifest identity, the exact file set, and every artifact
digest before it starts a replacement backend. Blaze takes ownership of that
backend before waiting for optional guest readiness and commits `Running` only
after a final liveness check. Corrupted or incomplete artifacts are refused
before any backend starts.

Failure handling follows the same "before the stop" boundary the restore
endpoint uses. A failure before Blaze begins stopping the backend resumes the
original runtime and leaves the sandbox `Running`, with one durability
exception: if persisting the hibernating intent crosses an uncertain boundary
(the state rename succeeds but its directory sync fails) or staging the image
fails after that point, the durable record may no longer agree with the live
runtime, so the sandbox is retained as `RecoveryRequired` for explicit
handling rather than reported as `Running`. A resume failure whose cleanup can
be confirmed returns the sandbox to `Hibernated` so the request can be retried;
when cleanup cannot be confirmed, the replacement owner and the operation
journal are retained through `RecoveryRequired` for explicit destroy.

Two durability properties are worth planning for. The storage slot stays
allocated for the whole hibernated period, and a successful resume keeps the
most recent hibernation image until the next hibernate replaces it or destroy
removes it — this trades disk space for a repeatable resume. After a daemon
restart, a completed hibernation is retained so it can still be resumed, but an
interrupted hibernate or resume is not completed automatically; it is retained
as `RecoveryRequired` and waits for explicit destroy.

## Storage Artifact Synchronization

Blaze can periodically persist the already-written host artifacts and directory
metadata owned by running sandboxes. The worker is disabled by default, so
existing deployments retain their previous behavior until an interval is
configured.

### Configuration

Set the interval and per-sandbox deadline in the daemon configuration:

```toml
[storage]
sync_interval = "30s"
sync_timeout = "10s"
```

`sync_interval = "disabled"` stops the periodic worker. `sync_timeout`
bounds how long the scheduler waits for one complete provider attempt:
reconstructing its storage slot and synchronizing that slot.

Each storage-provider synchronization call persists the already-written bytes
and directory metadata visible to that call. Concurrent artifact updates may
become visible in the current attempt or a later one.

### Runtime behavior

Each sweep selects sandboxes that are running and still own a complete storage
slot. A sandbox whose operation lock is already held is deferred without
waiting, allowing the sweep to continue to later sandboxes. Lifecycle changes,
guest requests, and storage artifact synchronization share this lock. After acquiring
an available lock, the worker rechecks lifecycle state before calling the
storage provider. A record that still says `Running` after the lock is acquired
but retains an unfinished operation or non-running backend ownership is
inconsistent and is reported as failed rather than deferred.
The first sweep starts after one complete configured interval. Missed timer
ticks are skipped instead of queued, preventing a slow sweep from accumulating
work.

A completed failure affects only that sandbox. Blaze retains storage ownership
and leaves lifecycle state unchanged, so a later sweep or destroy can retry.
If filesystem work cannot stop at the deadline, it keeps the sandbox operation
lock and the single synchronization permit until completion. Later attempts
are deferred instead of accumulating additional blocking work. Guest and
lifecycle operations that arrive while the lock is retained wait for the
provider work to finish; `sync_timeout` bounds scheduler waiting, not those
operations.

When the service loop stops, Blaze cancels and joins the periodic scheduler.
Provider work that cannot be cancelled remains under its sandbox lock until it
completes. Daemon-wide connection draining and runtime cleanup remain separate.

## Template Catalog

Blaze can atomically publish operator-prepared runtime artifacts and expose
their metadata through the daemon API. `/v1/templates` is the single
operator-facing template resource. A `POST /v1/sandboxes` request selects a
published entry through the optional `template` field, and the daemon restores
the new sandbox from that entry.

Sandbox creation resolves the optional template name from this same catalog;
there is no separate process-local registry for operators to configure or
monitor. The named entry must appear in the matched policy's `select.templates`
allow-list, and its recorded image, backend, version, and (for Firecracker) VM
and guest-transport shape must match what the policy would launch. Each
template-backed sandbox receives an independent copy of the artifacts, so it can
be checkpointed, rolled back, and deleted like any other sandbox without
affecting the catalog or its siblings.

### Configuration

The catalog directory has a default, but imports remain disabled until an
operator configures an import root:

```toml
[template]
dir = "/var/lib/blaze/templates"
import_root = "/var/lib/blaze/template-imports"
max_files = 32
max_bytes = 274877906944
max_metadata_bytes = 1048576
max_total_bytes = 1099511627776
max_entries = 128
```

Both roots must be absolute and disjoint from each other, from Blaze image,
instance, and policy roots, from every executable path configured in
`[backends]`, from the resolved location captured when the daemon configuration
file is opened for this startup, from that file's configured pathname, and from
the configured `daemon.socket` path and the host network coordination path
`/run/lock/blaze-network.lock`. They must also remain disjoint from the
conventional named network namespace trees `/var/run/netns` and `/run/netns`,
and from the fixed snapshot-view rootfs path
`/run/blaze-snapshot-view/rootfs.ext4`. Every Firecracker sandbox creates that
file as the bind-mount target for its own root filesystem, so a catalog root
configured at `/run/blaze-snapshot-view` — or reachable through a symbolic link
that resolves there — is rejected at startup rather than allowed to accumulate a
root-level file that catalog accounting would read as a malformed entry.
Relative `[backends]` paths are resolved once against the daemon's startup
working directory; boundary checks, backend probing, and sandbox launch then
reuse that absolute path. When a configured backend path is a symbolic link,
both the configured link location and its resolved target remain outside
template catalog ownership.
The same rule applies when the daemon configuration path is a symbolic link:
both the configured link location and the opened file's resolved location stay
outside template catalog ownership.
Template catalog roots must not contain symbolic link components. On Linux,
Blaze compares resolved path prefixes and their underlying filesystem locations
from the mount table, so symbolic-link and bind-mounted aliases cannot bypass
these directory boundaries. Blaze retains the opened configuration file and
rechecks its identity at the captured location, so retargeting the pathname
cannot substitute another configuration file. An overlap is rejected before catalog permissions are
changed or catalog entries are scanned. A template catalog root may use a
non-UUID child of `daemon.state_dir`, as the default does, but it cannot own the
state root or enter a sandbox UUID subtree.
If the catalog root does not exist yet, Blaze retains the deepest existing
parent directory and creates the missing suffix relative to that directory.
Startup stops if any planned component appears during validation, before Blaze
changes that object's permissions. Policy-entry boundary discovery follows
`policy.on_load_error`: a discovery failure in `warn` mode uses the same empty
policy engine as policy loading, while successfully discovered policy targets
remain protected. Executable files found through `PATH` for Blaze's host helper
commands are protected as well, including both their configured and resolved
locations.
Blaze retains the validated import-root directory opened at startup. Replacing
the configured pathname later does not redirect source lookup.

### Import and lookup

Publish a source directory below `import_root`:

```http
POST /v1/templates/import
Content-Type: application/json

{"name":"runtime-base","source":"runtime-base","description":"base runtime"}
```

`source` must be relative and must not traverse parent directories or links.
The source contains top-level regular files `vmstate.snap`, `mem.bin`, and
`rootfs.ext4`; `template.json` is optional and must be a JSON object. Source
directories and files must be owned by the daemon user and not writable by
group or other users. Nested directories, links, and special files are
rejected.

An entry that a create request will select must carry complete boot metadata in
`template.json`. Import itself only checks that the file is a JSON object, so an
entry without this metadata publishes successfully and is then rejected with
`409 Conflict` at create time:

| Field | Meaning |
|-------|---------|
| `format_version` | Must be `1` |
| `name` | Must equal the published catalog name |
| `image_digest` | Image identity the create request must also declare |
| `backend` | Backend that captured the snapshot |
| `backend_version` | Must equal the version the backend's restore adapter reports; `mock-v1` for the built-in Mock backend, and the exact capturing binary version for Firecracker |
| `boot_args` | Firecracker kernel command line captured in the snapshot; it must exactly match the selected policy's effective cold-start command line, including Blaze's fixed `ip=` argument when networking is enabled |
| `snapshot_kind` | Snapshot flavor, currently `full` |
| `expose_guest_socket` | Whether the captured runtime exposed the guest transport |
| `network` | Whether the captured runtime held a host network slot |
| `vcpus` / `memory_mib` | Firecracker VM shape captured in the snapshot; both must be non-zero and exactly match the selected policy |
| `rootfs_size` / `memory_size` | Byte sizes, must match `rootfs.ext4` and `mem.bin` |
| `artifacts` | Exactly three entries for `vmstate.snap`, `mem.bin`, and `rootfs.ext4`, each with `size_bytes` and a lowercase-hex `sha256` |

Create compares the manifest's `backend`, `backend_version`, and `snapshot_kind`
against what the selected backend's restore adapter reports, and a mismatch is
refused with `501 Not Implemented` even though the entry published successfully.
The status depends on where the problem is caught: a Firecracker manifest that
omits `backend_version` fails the manifest's own bootability rules first and is
refused with `409 Conflict`, while a Mock manifest that omits it satisfies those
rules and is refused with `501` by the adapter comparison.
For example, the built-in Mock adapter reports `mock-v1`; recording `mock-v2`
also returns `501`, which means the manifest value must be corrected rather
than selecting a different backend.

Firecracker entries additionally require `resource_layout = "portable-v1"`, a
present `boot_args` value, non-zero `vcpus` and `memory_mib`, and a `memory_size`
equal to `memory_mib` expressed in bytes. Those rules are also part of the
manifest's bootability check, so violating them yields `409 Conflict`. The
policy's effective cold-start kernel command line, VM shape, and guest-transport
settings must match these values exactly. When networking is enabled, the
effective command line includes the fixed `ip=` argument that Blaze appends.
Restore uses the command line captured in the snapshot rather than rebuilding it
from the current policy.
A missing or zero `vcpus`/`memory_mib`, or a VM shape that differs from the
policy, returns `409 Conflict` during preflight before lifecycle state or
storage allocation and therefore cannot leave a residual sandbox directory.

The built-in Mock backend does not restore guest transport or host networking,
so Mock entries must set both `expose_guest_socket` and `network` to `false`.
Requesting either unsupported resource is refused with `501 Not Implemented`
before any sandbox lifecycle state is written.

Template-backed create uses the same recoverable cleanup as ordinary create:

- Policy, image, backend, version, VM-shape, and guest-transport refusals occur
  before create intent or storage allocation. They return `409 Conflict` for a
  request or manifest conflict, or `501 Not Implemented` for an unsupported
  storage or restore capability, and leave no sandbox-owned storage.
- Copy, backend restore, guest-readiness, and final-state failures occur after
  create intent. Blaze first tries to stop the backend, release storage, and
  commit the sandbox as destroyed. If all compensation succeeds, it returns the
  original error and retains no sandbox resources.
- Incomplete compensation returns HTTP 500 with an error beginning `operation
  requires recovery`; the named sandbox remains in `RecoveryRequired` and may
  retain its storage or backend owner. Send
  `DELETE /v1/sandboxes/{id}` later to retry cleanup.

```json
{
  "format_version": 1,
  "name": "runtime-base",
  "image_digest": "sha256:...",
  "backend": "firecracker",
  "backend_version": "Firecracker v1.16.0",
  "resource_layout": "portable-v1",
  "boot_args": "console=ttyS0 reboot=k panic=1 pci=off",
  "snapshot_kind": "full",
  "expose_guest_socket": false,
  "network": false,
  "vcpus": 1,
  "memory_mib": 256,
  "rootfs_size": 536870912,
  "memory_size": 268435456,
  "artifacts": [
    {"name": "vmstate.snap", "size_bytes": 14174, "sha256": "..."},
    {"name": "mem.bin", "size_bytes": 268435456, "sha256": "..."},
    {"name": "rootfs.ext4", "size_bytes": 536870912, "sha256": "..."}
  ]
}
```

Every artifact is re-hashed against these values when a create request selects
the entry, so the digests must describe the published files exactly.

Published files must have exactly one hard link, and catalog entries and staging
directories must remain on the catalog root's mount. Blaze stops rather than
changing or traversing data that violates these boundaries.
Before startup scans or list/get reads open an artifact for reading, Blaze
classifies it without a read-capable handle and rechecks the opened object's
identity. On Linux, the readable handle is derived from the pinned classified
object, so replacing the directory entry cannot redirect the read.

Use `GET /v1/templates` to list sorted name-only summaries and
`GET /v1/templates/{name}` to read one entry's complete metadata. The
daemon validates entries one at a time while listing and retains at most one
list response until its body is released; a concurrent list request receives
`503 Service Unavailable`. It separately retains at most one complete item
response; another item request receives `503 Service Unavailable` until the
first response body is released. A duplicate name or a concurrent import of
the same name returns `409 Conflict`.

### Publication, limits, and recovery

Blaze enforces the configured per-entry file and byte limits while inspecting
input. It also reserves catalog bytes and one of the `max_entries` slots before
copying into a private staging directory. It rechecks source identity after
copying, synchronizes the complete entry, and publishes it with a no-replace
rename. Readers therefore see either no entry or a complete entry. Name-only
list responses cannot materialize more than the configured number of entries.

Failed imports remove their staging data, including a staging directory whose
post-creation open or validation fails. If cleanup or publication durability
cannot be confirmed, later imports are rejected until the catalog is repaired
and the daemon restarts. Startup validates published entries and removes owned
staging directories left by an interrupted import. Before either action, the
daemon obtains and retains an exclusive lock on the opened catalog root; a
second daemon using the same catalog fails before it can inspect or clean a live
import. Graceful shutdown rejects new imports, cancels active copies, and waits
for their file handles to close.

The API validates artifact structure, not whether a snapshot can boot with a
particular backend; boot compatibility is checked only when a create request
selects the entry. The catalog does not yet expose deletion or reference
tracking.
