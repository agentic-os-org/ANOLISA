# ws-ckpt and Session Management IPC Protocols

[中文版](../../zh/cosh-ng/ipc-protocol.md)

## Overview

cosh-ng communicates with the ws-ckpt daemon via Unix Domain Socket to manage workspace snapshots.
Communication uses a frame format of **bincode serialization + 4-byte little-endian length prefix**.

## Architecture

```
cosh-cli / cosh-core          ws-ckpt daemon
     │                              │
     │  Unix socket                 │
     │  /run/ws-ckpt/ws-ckpt.sock   │
     │─────────────────────────────→│
     │  [4B LE len][bincode req]    │
     │                              │
     │←─────────────────────────────│
     │  [4B LE len][bincode resp]   │
```

The client implementation is in `crates/cosh-platform/src/checkpoint.rs` (`CkptClient`),
and type definitions are in `crates/cosh-types/src/checkpoint.rs`.

## Frame Format

Each message consists of two parts:

```
┌──────────────────┬───────────────────────────────┐
│ 4-byte LE u32    │ bincode-encoded enum payload   │
│ (payload length) │ (WsCkptRequest / Response)     │
└──────────────────┴───────────────────────────────┘
```

- Length prefix: little-endian unsigned 32-bit integer representing the byte count of the subsequent bincode payload
- Maximum response limit: 64 MiB (prevents OOM)
- Default timeout: 5000ms (configurable via `CkptClient::with_timeout()`)

## Request Types (WsCkptRequest)

bincode serializes enums by variant index (first variant = index 0). **Variant order is the binary contract and must not be reordered.**

| Index | Variant | Description |
|-------|---------|-------------|
| 0 | `Init { workspace }` | Initialize a workspace |
| 1 | `Checkpoint { workspace, id, message, metadata, pin }` | Create a snapshot |
| 2 | `Rollback { workspace, to }` | Rollback to a specified snapshot |
| 3 | `Delete { workspace, snapshot, force }` | Delete a snapshot |
| 4 | `List { workspace, format }` | List snapshots |
| 5 | `Diff { workspace, from, to }` | Diff between two snapshots |
| 6 | `Status { workspace }` | Query status |
| 7 | `Cleanup { workspace, keep }` | Clean up old snapshots |
| 8 | `Config` | Get daemon configuration |
| 9 | `ReloadConfig` | Reload configuration |
| 10 | `Recover { workspace }` | Recover a workspace |
| 11 | `HealthAdvisory` | Health check |

## Response Types (WsCkptResponse)

| Variant | Corresponding Request | Key Fields |
|---------|----------------------|------------|
| `InitOk { ws_id }` | Init | Workspace ID |
| `CheckpointOk { snapshot_id }` | Checkpoint | Snapshot ID |
| `RollbackOk { from, to }` | Rollback | Rollback source and target |
| `DeleteOk { target }` | Delete | Deleted snapshot identifier |
| `Error { code, message }` | Any | Error code + human-readable description |
| `ListOk { snapshots }` | List | `Vec<SnapshotEntry>` |
| `DiffOk { changes }` | Diff | `Vec<DiffEntry>` |
| `StatusOk { report }` | Status | `StatusReport` |
| `CleanupOk { removed }` | Cleanup | List of removed snapshot IDs |
| `ConfigOk { config }` | Config | `ConfigReport` |
| `ReloadConfigOk` | ReloadConfig | No payload |
| `CheckpointSkipped { reason }` | Checkpoint | Skip reason (e.g., no changes) |
| `RecoverOk { workspace }` | Recover | Recovered workspace path |
| `HealthAdvisoryOk { ... }` | HealthAdvisory | Over-limit workspace count, disk usage |

## Error Codes (WsCkptErrorCode)

| Index | Variant | Description |
|-------|---------|-------------|
| 0 | `WorkspaceNotFound` | Workspace does not exist |
| 1 | `SnapshotNotFound` | Snapshot does not exist |
| 2 | `AlreadyInitialized` | Workspace already initialized |
| 3 | `BtrfsError` | Btrfs operation error |
| 4 | `IoError` | I/O error |
| 5 | `InvalidPath` | Invalid path |
| 6 | `ConfirmationRequired` | Confirmation needed (e.g., deleting a pinned snapshot) |
| 7 | `InternalError` | Internal error |
| 8 | `SnapshotAlreadyExists` | Snapshot ID conflict |
| 9 | `WriteLockConflict` | Write lock conflict |
| 10 | `DiskSpaceInsufficient` | Insufficient disk space |

## Client Usage

```rust
use cosh_platform::checkpoint::CkptClient;

// Default path /run/ws-ckpt/ws-ckpt.sock
let client = CkptClient::default_path();

// Or specify path and timeout
let client = CkptClient::with_timeout("/custom/path.sock", 10000);

// Health check
if !client.is_available() {
    eprintln!("ws-ckpt daemon not running");
}

// Operation examples
let result = client.create("/home/user/project", "snap-001", Some("initial"), None, false)?;
let list = client.list(Some("/home/user/project"))?;
let restored = client.restore("/home/user/project", "snap-001")?;
```

## Key Constraints

| Constraint | Description |
|------------|-------------|
| Variant order is immutable | bincode serializes enums by index; reordering breaks the wire format |
| New additions append only | New Request/Response variants can only be added at the end |
| Types must stay in sync | Definitions in `cosh-types` must exactly match `ws-ckpt-common` |
| Timeout handling | Client sets read/write timeouts to avoid blocking when daemon is unresponsive |
| Length limit | Responses exceeding 64 MiB are treated as anomalous; connection is dropped immediately |
| Socket path | Default `/run/ws-ckpt/ws-ckpt.sock`; overridable via environment variable or CLI argument |

## Test Verification

```bash
cd src/cosh-ng

# bincode round-trip serialization tests
cargo test --locked -p cosh-types -- checkpoint

# Variant index contract tests
cargo test --locked -p cosh-types test_request_bincode_variant_index

# CkptClient unit tests (no running daemon required)
cargo test --locked -p cosh-platform -- checkpoint
```

## cosh-core Session Management JSON Protocol

`cosh-core --session-control` is the stable internal boundary used by
cosh-shell to discover, validate, and clear provider conversations. It handles
one JSON request from standard input, writes one JSON response to standard
output, and exits. This mode loads configuration and the scoped session store
but does not initialize a provider, extensions, skills, hooks, or
authentication.

The caller must send the canonical workspace path it intends to manage. Core
canonicalizes the path again, derives its workspace-scoped store, and validates
every lowercase canonical UUID before constructing a session filename. Core
also loads the project configuration from
`<workspace>/.copilot-shell/config.toml`; it never uses the management
process's unrelated current directory for `session.auto_persist` or
`session.persist_dir`. Standard input is capped at 1 MiB. Oversized input,
invalid UTF-8, malformed JSON, and requests missing required fields return
`invalid_request` without initializing storage.

### Request Actions

Requests use an `action` discriminator:

```json
{"action":"list","workspace_scope":"/work/project","limit":20,"cursor":null,"all_workspaces":false}
{"action":"inspect","workspace_scope":"/work/project","session_id":"2d711642-b726-4b04-8d2a-8a0470f4ed24"}
{"action":"validate","workspace_scope":"/work/project","session_id":"2d711642-b726-4b04-8d2a-8a0470f4ed24"}
{"action":"prepare_clear_all","workspace_scope":"/work/project","protected_session_ids":[],"limit":4096,"cursor":null}
{"action":"clear","workspace_scope":"/work/project","session_ids":["2d711642-b726-4b04-8d2a-8a0470f4ed24"],"protected_session_ids":[]}
```

| Action | Contract |
|--------|----------|
| `list` | Returns newest-first summaries. `limit` defaults to 20 and is clamped to 1–100; pass the opaque `next_cursor` to read the next page. When `all_workspaces` is `true`, the shell requests a larger initial page size of 100 to reduce incomplete output, but it is still clamped to the same 1–100 core limit. Core enumerates every workspace-hash directory under the storage root, returns sessions from all workspaces, and marks foreign sessions with `scope_mismatch`. `all_workspaces` defaults to `false` for backward compatibility. |
| `inspect` | Returns a summary even when health prevents recovery. |
| `validate` | Fully loads the envelope and succeeds only for a resumable session. |
| `prepare_clear_all` | Returns a lexicographically paged clearable/protected ID plan without loading or transferring summaries. `limit` is clamped to 1–4096; continue with `next_cursor`. Omitting `limit` is accepted only when the complete plan fits one 4096-ID page, preventing older clients from silently accepting a partial plan. |
| `clear` | Deletes each requested ID independently and returns per-item skipped errors. Each request accepts at most 128 IDs. |

Summaries expose `session_id`, `workspace_scope`, creation and update times,
model, message count, first prompt, schema version, and one of these health
values: `ready`, `corrupt`, `incompatible`, or `scope_mismatch`.
The first-prompt preview is normalized to one line and capped at 160 Unicode
characters before serialization. Listing first orders UUID files by bounded
filesystem metadata, then reads only the requested page instead of
deserializing every history on every request. A session file is capped at
32 MiB for persistence, listing, validation, and resume; an oversized stored
entry is reported as `corrupt` without allocating its contents. Listing skips
an entry that disappears or cannot be read and continues scanning until the
requested page is full or no candidates remain, so a filtered first candidate
cannot hide healthy entries on the same page.
Cursors encode the last newest-first filesystem sort key, so deleting that
entry between page requests does not restart pagination.

### Response Envelope

Successful data is tagged with the matching action:

```json
{
  "ok": true,
  "data": {
    "action": "list",
    "sessions": [],
    "next_cursor": null
  }
}
```

A request-level failure exits with status 1:

```json
{
  "ok": false,
  "error": {
    "code": "not_found",
    "message": "session not found: 2d711642-b726-4b04-8d2a-8a0470f4ed24",
    "recoverable": true,
    "hint": "Refresh the session list and choose an existing entry."
  }
}
```

Stable error codes are `invalid_id`, `invalid_cursor`, `invalid_request`,
`not_found`, `io`, `corrupt`, `incompatible_version`, `scope_mismatch`,
`conflict`, and `active_session`.
They are recoverable for interactive callers. A `clear` request with per-item
failures still returns `ok: true`; failed entries appear in `data.skipped` with
their own typed errors.

`protected_session_ids` is mandatory defense-in-depth for interactive deletion.
cosh-shell sends both its selected and active provider IDs, and core refuses to
delete either one even if it is also present in `session_ids`. Both
`prepare_clear_all` and `clear` must include this field; an explicit empty
array confirms that the caller has no protected identities, while omission
rejects the complete request before any deletion.
The shell drains bounded `prepare_clear_all` ID pages to show the exact plan
before confirmation, then submits those IDs through 128-item `clear` batches;
it does not drain every summary page. Core rejects oversized direct batches,
and bounds per-item identifiers and error text by UTF-8 bytes. Core also
checks the complete serialized envelope against a 1 MiB hard budget, so
multi-byte input cannot bypass the client's response cap.
Each summary independently bounds untrusted model metadata to 256 UTF-8 bytes
and workspace metadata to 4096 UTF-8 bytes before page accumulation. A large
but otherwise valid session file therefore cannot make `list`, `inspect`, or
`validate` allocate an unbounded response or fall back to `invalid_request`.

If a later `clear` batch fails, the shell preserves confirmed `deleted` and
`skipped` results. The failed batch is reported as `unknown_session_ids`, and
IDs not yet sent are reported as `unattempted_session_ids`; the UI must not
collapse this state into a request-level error that hides earlier deletion.

The shell gives each one-shot management operation a single ten-second
deadline covering spawn, request-pipe writes, and response collection. Request
writes use a nonblocking pipe and are retried by the deadline-aware lifecycle
loop, so neither the leader nor a detached descendant can retain stdin and
block a bulk `clear` writer indefinitely. A timeout or transport failure
closes the request pipe, terminates the process group, escalates to forced
termination, waits for the leader, and joins all output workers.
Output workers use cancellable polling: after leader and process-group cleanup,
they drain bytes that are already readable and stop even if a descendant that
escaped the original process group still holds an inherited output descriptor.
An ordinary poll timeout repeats the poll and never enters a blocking read, so
a quiet leader cannot strand a reader before the lifecycle sets its stop flag.
The client also caps the JSON response at 1 MiB and stderr diagnostics at
256 KiB. Crossing either limit closes the pipe and terminates and reaps the
session-control process group.

### Persistence Compatibility

Schema-v1 envelopes contain the immutable provider session UUID, canonical
workspace, timestamps, model, optimistic generation, and model-visible
messages. Writes use a same-directory temporary file, file and directory
syncs, atomic rename, and a short-lived advisory lock. The kernel releases the
lock when its process exits, so an unlocked lock file is reusable rather than
treated as a conflict. A canonical workspace path must be valid UTF-8; Core
returns `invalid_request` before deriving a scope or storage hash when it is
not. Optimistic generations must advance monotonically, and a stored
`u64::MAX` generation is rejected without replacing history. On Unix,
scoped directories use mode `0700`; session,
temporary, and lock files use `0600`. Legacy raw message arrays load as
generation zero in memory. Core resolves symlinks in the storage root once,
when the store is constructed, then securely opens or creates every scoped
path component below that canonical root without following symlinks and pins
the workspace-hash directory. Symlinked home or dotfile layouts therefore
keep working while later symlink swaps below the root are refused.
Scoped enumeration, session and lock opens, temporary-file creation, atomic
rename, and removal are all relative to that descriptor; session and lock opens
use `NOFOLLOW`. Replacing a workspace hash directory with another workspace's
symlink therefore cannot redirect `load`, `persist`, `list`, or `clear`.
Clearing a session also removes its paired lock file, and stale temporary
files from crashed writers are swept when the directory is next opened for
writing.

An explicit legacy lookup by canonical UUID checks only a
former flat directory whose ownership by the requested workspace can be
established. With the new default root, that means the requested workspace's
former relative `sessions/` directory. A custom root is eligible for legacy
lookup only when its configured value is workspace-relative, contains no `..`
component, already exists as a directory, and resolves through symlinks inside
the canonical workspace. Core opens the canonical workspace one path component
at a time without following symlinks, then pins each eligible legacy directory
with an open descriptor. Legacy enumeration, session opens, and removal remain
relative to that descriptor; session opens use `NOFOLLOW`. A concurrent
rename-and-symlink replacement therefore cannot redirect load or clear outside
the pinned workspace-owned directory. Absolute, `~/`, and parent-escaping roots
do not participate in legacy lookup; they may name scoped storage, but scoped
access rejects symlinks in every path component below the canonical storage
root. Core never infers legacy
ownership from directory-prefix containment and does not inspect the process
cwd or an ambiguous shared flat root. Workspace-owned legacy sessions appear
in `list` summaries beside scoped envelopes, so the picker, `prepare_clear_all`,
and explicit `clear` observe the same population; ambiguous files outside an
established workspace-owned directory are never claimed, listed, or rewritten
by `inspect`, `validate`, or load. Explicit `clear` can remove even a
corrupt legacy file while still honoring protected IDs. Migration locks the
legacy source, atomically writes a schema-v1 envelope into the requested
workspace scope, and then removes the old file. A legacy cleanup failure is
reported as a typed persistence error and retried by later persists. When both
copies exist, clear removes the legacy copy first, so a legacy permission
failure can never delete the newer scoped history or resurrect stale content.

The JSONL headless protocol and this management protocol share
`SessionStore::load`; interactive selection cannot bypass the validation used
by direct `cosh-core --resume`.

A session-load failure is explicit on the JSONL result:

```json
{"type":"result","is_error":true,"errors":["session recovery failed [not_found]: session not found"],"session_error_code":"not_found","session_error_phase":"load","session_id":"..."}
```

cosh-shell distinguishes selected recovery from automatic continuation of an
active provider session. A Core session failure carries separate
`session_error_code` and `session_error_phase` fields. A `load` failure with
`not_found`, `corrupt`, `incompatible_version`, or `scope_mismatch`, or any
typed `persist` failure, releases only the matching attempted identity. An
owned selected attempt becomes `failed` while retaining the structured code,
provider message, and phase-specific recovery hint; its previous active UUID
remains available. Provider error text containing bracketed words cannot
impersonate a session failure. The provider-reported ID must also match both
selected and active resume attempts before Core identity is committed.
Unrelated selected IDs remain selected. A one-turn
`disable provider resume` hint omits `--resume` without consuming a pending
user selection.

The JSONL `system/init` message includes `session_resumable`. A value of
`false` means `session.auto_persist` is disabled: consumers must not capture
the reported UUID and must invalidate an identity only when that identity was
actually carried by this invocation's `--resume`. A fresh one-turn fallback
therefore cannot consume an unrelated selected ID or an older active ID. The
rule also applies when the turn later fails, is cancelled, or exits
abnormally. The field is optional for compatibility with providers that do
not implement this extension; absence retains the existing session-ID capture
behavior.

The active ID, workspace, and invocation generation share one state lock.
Every started turn receives a generation token, and success, failure,
cancellation, non-resumable cleanup, and identity mismatch transitions commit
only while that token still owns the latest attempt. A cancelled worker that
finishes late therefore cannot clear or overwrite a newer turn, including a
retry of the same selected ID. Structured session results are finalized before
any subsequent transport failure is delivered. Replacing or rejecting a
selection also advances the generation atomically. A fresh turn that does not
carry `--resume` releases a superseded `restoring` owner back to `selected`,
and cancellation applies any already parsed structured session failure before
delivering `AgentCancelled`. Destructive session management holds the same
state lease for the complete clear operation, while selection holds it across
validation and commit. Clear and activation are therefore linearized rather
than relying on a stale snapshot of protected IDs.

### Test Verification

```bash
cd src/cosh-ng
cargo test --package cosh-core
cargo test --package cosh-shell --test protocol
```
