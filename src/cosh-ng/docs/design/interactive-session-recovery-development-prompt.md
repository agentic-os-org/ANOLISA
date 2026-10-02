# Development Prompt: Interactive Session Recovery

Implement [alibaba/anolisa#1546](https://github.com/alibaba/anolisa/issues/1546)
end to end in the current ANOLISA repository.

Do not stop after analysis or produce only a design. Implement the feature,
tests, and required documentation. Treat
`src/cosh-ng/docs/design/interactive-session-recovery-plan.md` as the
authoritative implementation plan for this task.

## Required reading

Before editing:

1. Read the repository `AGENTS.md`.
2. Read `src/cosh-ng/AGENTS.md` completely.
3. Read
   `src/cosh-ng/docs/design/interactive-session-recovery-plan.md` completely.
4. Read `specs/documentation-standard.md` before changing documentation.
5. Inspect the current implementations in:
   - `src/cosh-ng/crates/cosh-core/src/session.rs`
   - `src/cosh-ng/crates/cosh-core/src/headless.rs`
   - `src/cosh-ng/crates/cosh-core/src/core.rs`
   - `src/cosh-ng/crates/cosh-core/src/protocol.rs`
   - `src/cosh-ng/crates/cosh-shell/src/adapter/cosh_core.rs`
   - `src/cosh-ng/crates/cosh-shell/src/adapter/cosh_core_process.rs`
   - `src/cosh-ng/crates/cosh-shell/src/adapter/control_protocol.rs`
   - `src/cosh-ng/crates/cosh-shell/src/runtime/state.rs`
   - `src/cosh-ng/crates/cosh-shell/src/runtime/cli_args.rs`
   - `src/cosh-ng/crates/cosh-shell/src/runtime/controller.rs`
   - `src/cosh-ng/crates/cosh-shell/src/raw_input/`
   - `src/cosh-ng/crates/cosh-shell/src/slash/`
6. Use these copilot-shell files only as behavioral references:
   - `deprecated/copilot-shell/packages/core/src/services/chatRecordingService.ts`
   - `deprecated/copilot-shell/packages/core/src/services/sessionService.ts`
   - `deprecated/copilot-shell/packages/cli/src/ui/components/SessionPicker.tsx`
   - `deprecated/copilot-shell/packages/cli/src/ui/hooks/useSessionPicker.ts`
   - `deprecated/copilot-shell/packages/cli/src/ui/hooks/useResumeCommand.ts`
   - `deprecated/copilot-shell/packages/cli/src/config/config.ts`

Verify current code instead of assuming the issue statement reflects runtime
behavior.

## Objective

Users must be able to discover, inspect, select, resume, continue, and safely
clear persisted cosh-core sessions from cosh-shell. Interactive recovery and
direct `--resume` must use one core-owned persistence and validation
implementation.

The completed behavior must include:

- real session persistence from production cosh-core paths;
- real message-history loading for `cosh-core --resume <id>`;
- versioned storage and legacy-array compatibility;
- validated session IDs and path traversal protection;
- atomic writes and conflict protection;
- stable typed errors for missing, corrupt, incompatible, scope-mismatched,
  conflicting, and protected sessions;
- a core-owned one-shot JSON management protocol for list, inspect/validate,
  and clear;
- `/session`, `/session status`, `/session resume <id>`, and confirmed clear
  flows in cosh-shell;
- a keyboard-driven session picker using the existing raw-input/card
  architecture;
- selected/restoring/active/failed recovery state;
- launch-time `cosh-shell --resume [id]`;
- active and selected session deletion protection;
- unit, protocol, raw CLI, and real-PTY coverage;
- required English and Chinese documentation.

## Critical correctness constraints

1. Fix core semantics before implementing the picker. The current store is
   unused, and current `--resume` only changes an ID.
2. Fix the control-protocol `"default"` overwrite. A missing control-protocol
   ID is unspecified and must not replace the initialized or resumed provider
   session ID.
3. Keep PTY shell session IDs separate from provider conversation IDs.
   Recovery must not reuse historical IDs for command blocks or terminal
   evidence.
4. cosh-shell must not deserialize full persisted core history. It may consume
   summaries and typed management outcomes only.
5. Historical terminal output is not restored. Surface that limitation in
   status instead of leaving stale evidence references looking usable.
6. Reject unvalidated IDs before constructing filesystem paths.
7. Missing, corrupt, incompatible, and raced-away sessions are non-fatal to
   cosh-shell.
8. Clearing requires explicit confirmation and must protect both the selected
   and active provider sessions in shell and core layers.
9. Do not add a new root `crates/cosh-shell/src/*.rs` implementation file.
   Place new shell code under an existing owner directory.
10. Do not introduce `mod.rs`, commented-out code, bare TODOs, or
    `unwrap`/`expect` in production library-style paths.
11. Do not add a dependency until checking workspace dependencies and existing
    equivalents. Declare third-party versions at workspace level.
12. Preserve unrelated user changes and do not commit, push, or open a PR
    unless explicitly requested.

## Implementation order

### 1. Establish the core contract

- Replace the raw message array with a versioned session envelope.
- Implement typed session identifiers, summaries, health, and errors.
- Resolve deterministic workspace-scoped storage while respecting an explicit
  configured directory.
- Add atomic persistence and concurrency/conflict protection. Legacy cleanup
  failure must remain visible and must never remove a newer scoped copy first.
- Support legacy JSON arrays as schema v0 and upgrade them after a successful
  write. Discover them only from explicit workspace-owned sources: the former
  default `<workspace>/sessions` directory or a workspace-relative custom
  root that contains no parent traversal and resolves inside the canonical
  workspace, never from prefix containment of an absolute/shared root.
- Expose list, inspect/validate, and protected clear through a provider-free
  one-shot JSON mode. Bound clear-all plans and clear results independently of
  client output limits through pagination and request batching. Bound text by
  UTF-8 bytes, require an explicit protected-ID set for every deletion request,
  and enforce hard byte budgets on both input and the serialized response.

### 2. Wire production persistence and resume

- Load the session before system initialization and hooks.
- Initialize both session ID and messages from the loaded record.
- Keep the provider session identity immutable during the process.
- Persist committed state after every turn boundary that may have changed
  history, including recoverable error paths.
- Add a two-process integration test proving the second invocation resumes the
  first invocation's model-visible history.
- Do not continue until this test passes.

### 3. Add the cosh-shell management client and state

- Add a child module under the cosh-core adapter owner for the one-shot
  management client.
- Model recovery as `selected`, `restoring`, `active`, or `failed`.
- Store active ID, workspace, recovery selection, and attempt generation under
  one state lock. Every runner terminal path must compare-and-commit its exact
  generation so a cancelled worker cannot mutate a newer attempt.
- Advance the generation when selection succeeds or fails. A fresh invocation
  that supersedes `restoring` must return the selection to `selected`, and a
  cancelled runner must apply already parsed structured session errors before
  emitting cancellation.
- Map every core session error to a recoverable shell notice.
- Preserve completed, unknown, and unattempted IDs when a later clear batch
  fails after earlier batches changed disk state.

### 4. Add commands and launch options

- Register and parse the `/session` command family.
- Implement list, status, explicit resume, and explicit clear behavior.
- Add `cosh-shell --resume <id>` and no-value `--resume`.
- Refactor launch option parsing sufficiently that option values are never
  mistaken for adapter names.
- Keep any `/resume` alias routed to the same implementation.

### 5. Add picker and clear confirmation

- Extend the existing raw-input capture/event bridge for the session manager.
- Provide navigation, selection, cancellation, multi-selection, and clear
  confirmation.
- Preserve prompt, terminal mode, and shell usability on all success and error
  exits.
- Show corrupt and incompatible entries but prevent their selection for
  resume. Permit confirmed deletion.

### 6. Complete tests and documentation

- Implement the full test matrix in the authoritative plan.
- Use isolated HOME directories, stores, and mock providers.
- Update component README summaries in English and Chinese.
- Add equivalent English and Chinese user-guide reference content and link it
  from the cosh-ng QUICKSTART entry points.
- Update developer IPC documentation if the session-control protocol is
  documented as stable.
- Do not update CHANGELOG outside a release version-bump change.

## Verification

Run focused tests while developing, then the required gates:

```bash
cd src/cosh-ng
cargo test --package cosh-core
cargo test --package cosh-shell --lib
cargo test --package cosh-shell --test logic
cargo test --package cosh-shell --test protocol
cargo test --package cosh-shell --test raw_cli <relevant-test> -- --exact
cargo test --package cosh-shell --test shell_host -- --test-threads=4
crates/cosh-shell/scripts/check-layout.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

If an environment problem blocks a command, capture the exact command and
error. Continue with all other meaningful checks. Do not weaken or delete
tests to pass a gate.

## Completion report

Return a concise implementation report containing:

- the resulting user-visible behavior;
- core storage/protocol decisions;
- the exact files changed;
- tests and gates run with outcomes;
- any remaining limitation or environment blocker;
- confirmation that every issue acceptance criterion is covered.

Do not claim completion while required behavior or tests remain missing.
