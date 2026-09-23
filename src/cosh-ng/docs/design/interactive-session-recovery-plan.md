# Interactive Session Recovery Implementation Plan

Status: Proposed

Issue: [alibaba/anolisa#1546](https://github.com/alibaba/anolisa/issues/1546)

Verified against `main` at `c5bd2556c909` on 2026-07-17.

## Summary

Implement interactive session recovery by making `cosh-core` the sole owner of
session persistence, validation, compatibility, and deletion semantics, then
exposing those semantics to `cosh-shell` through a structured internal
management protocol.

Do not begin with the picker. The current `SessionStore` is not connected to
the runtime, `--resume` only changes the session identifier, and the
cosh-core control-protocol path can overwrite that identifier with
`"default"`. Core correctness must land before interactive recovery can be
trusted.

The copilot-shell implementation is a behavioral reference, not code to port
verbatim. Reuse its metadata, pagination, project scoping, preview, and picker
concepts. Keep cosh-ng's Rust/PTTY architecture and avoid importing its
React/Ink UI-history model.

## Goals

- Persist enough cosh-core conversation state to continue a previous task.
- List recoverable sessions with useful metadata and explicit health.
- Resume a selected session through the same load path as `--resume`.
- Show the selected, restoring, active, and failed recovery states.
- Clear one or more sessions only after confirmation.
- Never clear the selected or active provider session accidentally.
- Treat missing, corrupted, incompatible, and scope-mismatched sessions as
  recoverable user-facing errors.
- Cover storage, protocol, shell logic, raw CLI, and real-PTY behavior.

## Non-goals

- Restoring the historical PTY process, shell jobs, environment, or terminal
  scrollback.
- Making old `terminal-output://` references readable in a new shell session.
- Porting copilot-shell's React/Ink components or UI transcript replay.
- Adding conversation branching, rename, compression checkpoints, or cloud
  synchronization in the first implementation.
- Unifying agent conversation recovery with ws-ckpt workspace snapshots.

## Current-State Findings

### cosh-core

- [`session.rs`](../../../src/cosh-ng/crates/cosh-core/src/session.rs) can write, read, list,
  and remove a raw `Vec<Message>`, but no production caller constructs a
  `SessionStore`.
- [`headless.rs`](../../../src/cosh-ng/crates/cosh-core/src/headless.rs) handles `--resume`
  by assigning `engine.session_id`; it does not load `engine.messages`.
- No runtime path persists `engine.messages` after a turn.
- The default persistence directory is the relative path `sessions`, making
  its meaning dependent on process cwd.
- Session identifiers are interpolated into filenames without validation.
- Listing returns only IDs, is unsorted, and silently converts directory
  failures into an empty list.
- Session errors are strings and cannot distinguish missing, corrupted,
  incompatible, invalid, or out-of-scope data.

### cosh-shell

- [`adapter/cosh_core.rs`](../../../src/cosh-ng/crates/cosh-shell/src/adapter/cosh_core.rs)
  automatically passes the committed provider session ID through
  `--resume`, scoped to an exact cwd, but cannot select a historical ID.
- [`adapter/cosh_core_process.rs`](../../../src/cosh-ng/crates/cosh-shell/src/adapter/cosh_core_process.rs)
  serializes its user message with no session ID.
- [`adapter/control_protocol.rs`](../../../src/cosh-ng/crates/cosh-shell/src/adapter/control_protocol.rs)
  replaces a missing ID with `"default"`.
- [`headless.rs`](../../../src/cosh-ng/crates/cosh-core/src/headless.rs) accepts that value and
  overwrites the engine session ID. Auto and trust modes therefore lose the
  startup or resumed identity on the main control-protocol path.
- `/debug session` exposes diagnostic provider state, but there is no
  user-facing session command, picker, recovery state, or clear confirmation.
- Existing mode/config panels demonstrate the correct inline card and
  raw-input pattern for a Rust/PTTY implementation.

### copilot-shell reference

Useful patterns:

- [`chatRecordingService.ts`](../../../../deprecated/copilot-shell/packages/core/src/services/chatRecordingService.ts)
  records version, cwd, timestamps, branch, message relationships, and exact
  model history.
- [`sessionService.ts`](../../../../deprecated/copilot-shell/packages/core/src/services/sessionService.ts)
  separates listing, loading, preview, existence, and removal operations.
- [`SessionPicker.tsx`](../../../../deprecated/copilot-shell/packages/cli/src/ui/components/SessionPicker.tsx)
  provides sorted selection, pagination, branch filtering, and previews.
- [`useResumeCommand.ts`](../../../../deprecated/copilot-shell/packages/cli/src/ui/hooks/useResumeCommand.ts)
  treats resume as an explicit state transition that resets active UI/core
  state.
- CLI configuration uses the same service for `--continue` and `--resume`.

Patterns not sufficient for this issue:

- Corrupted files are frequently skipped or converted to `undefined`.
- Removal is not wired to a confirmation UI and does not protect the active
  session.
- The React/Ink components do not fit cosh-shell's raw-input card pipeline.
- UI transcript reconstruction contains copilot-shell-specific records and
  should not be copied.

## Architecture

### Identity boundaries

Keep these identities separate:

- `ShellSessionId`: the PTY/OSC session used by command blocks, evidence, and
  `terminal-output://` references.
- `ProviderSessionId`: the cosh-core conversation persisted across agent turns.
- `RunId`: one provider invocation.

Interactive recovery changes only `ProviderSessionId`. Reusing a historical
provider ID as `ShellSessionId` would make new command evidence collide with
stale terminal references.

Use validated newtypes or equivalent constructors at file and protocol
boundaries. A valid provider session filename is a canonical UUID and cannot
contain separators, dot segments, or arbitrary suffixes.

### Workspace scope

Derive a stable workspace scope from the canonical cwd supplied by the shell
request. Store it in every session and use it consistently for:

- list filtering;
- resume validation;
- adapter continuation gating;
- clear authorization.

Preserve the current safety property that a session is not silently resumed in
an unrelated cwd. If the implementation elects to use a repository root
instead of exact cwd, apply the same resolver in both core and shell and add
tests for subdirectories and non-git directories.

### Persisted session

Replace the unversioned message array with a versioned envelope similar to:

```text
PersistedSession
  schema_version
  session_id
  workspace_scope
  created_at
  updated_at
  model
  generation
  messages
```

Derive display metadata from the envelope:

- first non-empty user prompt;
- message count;
- model;
- created and updated timestamps;
- workspace scope;
- file modification time as a fallback.

Write the full envelope to a sibling temporary file and atomically rename it
over the destination. Serialize before opening the destination. Use a
per-session lock or optimistic generation check so two processes cannot
silently overwrite divergent histories.

Respect an explicitly configured persistence directory. Replace the default
relative `sessions` location with a deterministic user-data location scoped by
workspace. On explicit resume, support the legacy JSON-array format as schema
v0 and upgrade it after the next successful write. Do not silently accept a
legacy file from a different workspace when its scope cannot be established.

### Typed outcomes

The session layer must distinguish:

- `InvalidId`
- `NotFound`
- `Io`
- `Corrupt`
- `IncompatibleVersion`
- `ScopeMismatch`
- `Conflict`
- `ActiveSession`

Each management response includes a stable machine-readable code, a concise
message, whether the failure is recoverable, and a user-action hint where
useful.

Listing should not crash or silently hide malformed entries. Return a summary
with a health value such as `ready`, `corrupt`, or `incompatible` when the file
can be safely identified. A corrupt entry remains eligible for confirmed
deletion but not resume.

### Core-owned management protocol

Add a private, structured one-shot mode such as:

```text
cosh-core --session-control
```

It accepts one JSON request on stdin and returns one JSON response on stdout.
Required actions:

- `list`: scoped, sorted, bounded, cursor-based summaries;
- `inspect`: summary and health for one ID;
- `validate`: run the same ID, schema, and workspace checks as resume;
- `clear`: delete an explicit set while honoring a protected session ID.

Do not return full message history to cosh-shell. The interactive shell needs
metadata and validation only; `cosh-core --resume` remains responsible for
loading model history.

The management mode must avoid provider initialization, authentication, hooks,
skills, and model calls.

### Resume lifecycle

For direct `cosh-core --resume ID`:

1. Resolve the workspace and store.
2. Validate and load the persisted session.
3. Initialize `engine.session_id`, `engine.messages`, and compatible metadata.
4. Emit `system init` only after load succeeds.
5. Reject any later control-protocol session ID that conflicts with the
   initialized identity. Treat legacy `"default"` as unspecified, not as a
   real ID.
6. Run the turn.
7. Persist the resulting committed message history at the turn boundary,
   including recoverable error paths that have already mutated history.

For interactive selection:

1. Ensure no provider run or destructive panel is active.
2. Use the core management protocol to validate the selected summary.
3. Store the selected provider ID and workspace scope in `CoshCoreAdapter`.
4. Set recovery state to `selected`.
5. On the next agent request, pass the ID through `--resume` and mark
   `restoring`.
6. Mark the session `active` only after a successful provider completion.
7. On load failure, keep the shell alive, show the typed error, and mark the
   recovery state `failed`.

### Interactive UX

Use `/session` as the unified public entry point:

```text
/session
/session status
/session list
/session resume <id>
/session clear <id>...
/session clear --all
```

`/session` opens the manager. A hidden or compatibility `/resume` alias may
open the same manager; it must not implement separate semantics.

The manager should show:

- selection cursor;
- first prompt or fallback ID;
- relative updated time;
- message count;
- workspace/model where space permits;
- `ready`, `corrupt`, or `incompatible` health;
- whether an item is selected or active.

Minimum keys:

- Up/Down or `j`/`k`: move;
- Enter: select/resume a ready session;
- Space: toggle clear selection;
- `d`: open clear confirmation;
- Esc/Ctrl+C: cancel without changing session state.

The clear confirmation lists the exact IDs/count. The active and selected
provider sessions are excluded in both shell logic and the core clear request.
If `--all` is requested, report protected sessions as skipped.

Expose a user-facing status containing:

- shell session ID;
- provider session ID;
- workspace scope;
- recovery state;
- last recovery error, if any;
- explicit notice that historical terminal evidence was not restored.

### CLI behavior

Keep `cosh-core --resume <id>` as the backend contract and make it perform a
real load.

Add shell launch parsing without breaking adapter detection:

```text
cosh-shell --resume <id>
cosh-shell --resume
```

The first validates and preselects a session. The second opens the manager
after `ShellReady`. Replace ad hoc option skipping with a launch-options
structure so a resume value cannot be mistaken for an adapter name.

`--continue` may be added only after deterministic newest-session ordering is
covered by tests. It is not required for the first acceptance pass.

## Implementation Phases

### Phase 1: Core correctness

Primary files:

- `crates/cosh-core/src/session.rs`
- `crates/cosh-core/src/cli.rs`
- `crates/cosh-core/src/headless.rs`
- `crates/cosh-core/src/core.rs`
- `crates/cosh-core/src/protocol.rs`
- `crates/cosh-core/src/config.rs`
- `crates/cosh-shell/src/adapter/control_protocol.rs`
- `crates/cosh-shell/src/adapter/cosh_core_process.rs`

Deliver:

- versioned, atomic, validated store;
- legacy v0 reader;
- real load and persistence;
- immutable control-protocol identity;
- typed management protocol;
- core and protocol tests.

Do not begin shell UI work until a two-process test proves that a second
cosh-core invocation can see and continue the first invocation's messages.

### Phase 2: Discovery and resume

Primary files:

- `crates/cosh-shell/src/adapter/cosh_core.rs`
- a child module under `crates/cosh-shell/src/adapter/cosh_core/` for session
  management client logic;
- `crates/cosh-shell/src/slash/parser.rs`
- `crates/cosh-shell/src/slash/commands.rs`
- `crates/cosh-shell/src/slash/registry.rs`
- `crates/cosh-shell/src/slash/session.rs`
- `crates/cosh-shell/src/runtime/state.rs`
- `crates/cosh-shell/src/runtime/cli_args.rs`
- i18n message definitions and catalogs.

Do not add a new root `crates/cosh-shell/src/*.rs` implementation file.

Deliver:

- management client;
- `/session` list, resume, and status;
- adapter recovery state;
- launch-time `--resume`;
- logic and protocol tests.

### Phase 3: Picker, clear, and PTY hardening

Primary files:

- `crates/cosh-shell/src/raw_input/mode.rs`
- `crates/cosh-shell/src/raw_input/mod.rs`
- `crates/cosh-shell/src/raw_input/card_capture.rs`
- `crates/cosh-shell/src/raw_input/capture_bridge.rs`
- `crates/cosh-shell/src/shell_host/raw_relay.rs`
- `crates/cosh-shell/src/runtime/controller.rs`
- `crates/cosh-shell/src/slash/session.rs`
- `crates/cosh-shell/tests/logic.rs` child modules
- `crates/cosh-shell/tests/protocol.rs` child modules
- `crates/cosh-shell/tests/raw_cli.rs` child modules
- `crates/cosh-shell/tests/shell_host.rs` child modules.

Deliver:

- keyboard picker;
- multi-select clear confirmation;
- active-session protection;
- corrupt/incompatible notices;
- raw CLI and real-PTY acceptance tests.

## Test Matrix

### cosh-core unit tests

- versioned persist/load round trip;
- metadata derivation and newest-first ordering;
- legacy v0 load and upgrade;
- invalid ID and path traversal rejection;
- missing session;
- malformed JSON;
- unsupported schema version;
- workspace mismatch;
- atomic write preserves the prior good file on failure;
- conflict/lock behavior;
- clear one, clear many, and protected-session rejection.

### cosh-core integration tests

- first invocation persists a new session;
- second invocation with `--resume` loads prior messages;
- a mock provider observes restored history;
- control-protocol `"default"` cannot replace the real session ID;
- persistence occurs after a partially mutating recoverable error;
- management mode does not initialize providers or require auth.

### cosh-shell logic/protocol tests

- `/session` parsing, registry visibility, hints, and status;
- management response parsing for every health/error code;
- adapter transitions `none -> selected -> restoring -> active`;
- failed recovery remains recoverable;
- cwd scope mismatch;
- active/selected clear protection;
- launch option parsing for `--resume`, `--shell`, and adapter combinations.

### raw CLI and real-PTY tests

- open, navigate, select, and cancel the picker;
- empty-list behavior;
- corrupt and incompatible entries;
- resume then continue with the mock provider;
- multi-select deletion;
- confirmation cancel leaves all files;
- confirmed clear skips the active session;
- missing file between list and selection;
- terminal mode and prompt are restored after every exit path.

Use isolated `HOME`, persistence directories, and mock providers. Tests must
not read or delete a developer's real sessions.

## Documentation Work Required With Implementation

Because the implementation adds CLI flags and slash commands, the same change
must update:

- `src/cosh-ng/README.md`
- `src/cosh-ng/README_zh.md`
- the cosh-ng user-guide entry point under
  `docs/user-guide/{en,zh}/user-entrypoint/cosh-ng/`;
- a full English and Chinese session-recovery reference in that user-guide
  directory;
- developer IPC documentation if `--session-control` becomes a stable
  internal protocol.

Do not update CHANGELOG files outside a release version-bump change.

## Acceptance Mapping

| Issue criterion | Planned evidence |
|---|---|
| List and select historical sessions | Scoped summaries plus PTY picker |
| Restore supported task context | Real core message load before model initialization |
| Interactive and `--resume` share rules | Both enter the same core `SessionStore::load` path |
| Confirmed clear protects active session | Two-layer protection and PTY tests |
| Missing/corrupt/incompatible is recoverable | Typed outcomes, health badges, non-fatal notices |
| Persistence/list/resume/clear/PTY tests | Test matrix above |

## Definition of Done

- Every issue acceptance criterion has an automated test.
- No production session path accepts an unvalidated ID.
- No shell code deserializes persisted core message history.
- A selected historical session continues with prior model context.
- The shell remains usable after every recovery or clear failure.
- Active and selected sessions cannot be deleted interactively.
- Old terminal evidence is not represented as restored.
- `cargo fmt --all -- --check` passes.
- `cargo clippy --workspace --all-targets -- -D warnings` passes.
- `cargo test --workspace` passes.
- `cargo doc --workspace --no-deps` passes when public API or rustdoc changes.
- `crates/cosh-shell/scripts/check-layout.sh` passes without new debt.
- Required English and Chinese user documentation is updated.

## Code Organization Notes and Waivers

- `cosh-core/src/session/store.rs` stays a focused owner: its inline test
  suite lives in `session/store/tests.rs` and workspace-owned legacy
  discovery/locking/removal lives in `session/store/legacy.rs`, keeping the
  production owner file below the 1000-line no-growth threshold.
- The twelve `adapter::Session*` contracts re-exported through the adapter
  public surface are classified `private-candidate` in
  `crates/cosh-shell/scripts/inventory-public-api.sh`: their only external
  consumers are this crate's integration tests, matching the existing
  `CoshCoreAdapter` classification. They must be made crate-private once
  those tests migrate to an internal harness path, and must not be frozen
  as stable API without a separate review.
- Waiver, `cosh-core/src/protocol.rs` (owner: session recovery feature):
  the file was already over the 1000-line no-growth threshold on base; this
  feature adds only the `session_error_code` / `session_error_phase` /
  `session_resumable` fields and the `session_result_error` constructor,
  which cannot leave the `OutputMessage` protocol owner without splitting
  the shared JSONL contract enum itself. Re-split condition: the next
  change that adds a new protocol message family must extract a
  `protocol/session.rs` (or equivalent) owner module first.
- Waiver, `cosh-core/src/config.rs` (owner: session recovery feature): the
  file was already over the 1000-line threshold on base; this feature adds
  three lines (`DEFAULT_SESSION_PERSIST_DIR` and workspace-scoped config
  loading) that belong to the existing config owner. Re-split condition:
  the next change that adds a new `[session]` or provider config section
  must extract the session config types into their own module first.
