# AGENTS.md — blaze

> Common Rust conventions (comments, module layout, dependency management, error handling, pre-commit checks, commit conventions) are defined in the [root AGENTS.md](../../AGENTS.md). This file documents **blaze-specific** additions only.

## Architecture

blaze is a **daemon-only** per-host sandbox orchestrator. All sandbox management is exposed via HTTP API; the binary only handles daemon lifecycle (start / reload / doctor).

Two-crate workspace:

- **blaze-core** (library): policy engine, lifecycle state machine, backend selector, kernel hook registry, config schema. Zero I/O beyond local TOML/JSON parsing.
- **blazed** (binary): daemon HTTP server (UDS + TCP), spawner implementations, metrics endpoint, CLI for daemon lifecycle commands.

Dependency direction: `blazed` → `blaze-core`. No reverse dependency.

## Build & Test

```bash
cd src/blaze
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

Platform: Linux (x86_64 + aarch64) for production. macOS builds succeed but spawners auto-downgrade to `MockSpawner`.

## Key Design Constraints

- **Daemon-only API model**: No CLI client for sandbox operations. All instance and template management is done via HTTP endpoints on UDS (`/run/blaze/api.sock`) or TCP (`:14159`). The CLI subcommands (`daemon start`, `daemon reload`, `daemon doctor`) only manage daemon lifecycle.
- **BackendSpawner trait**: All backend-specific process management is behind `BackendSpawner` (`spawn`, `probe`, `cleanup_orphan`, and defaulted `restore`/`restore_capability`) and `BackendInstance` (`backend`, `try_wait`, `kill`, plus defaulted `pause`, `resume`, `snapshot`, and the capture-orchestration hooks `quiesce_for_capture`/`unquiesce_after_capture`, which delegate to pause/resume and are overridden as no-ops by backends whose capture primitive freezes the workload itself; the quiesce must hold until `unquiesce_after_capture`, because storage synchronization and rootfs capture run after `snapshot` returns). Adding a new backend means implementing the required methods and registering it in `daemon::build_spawners()`.
- **Policy-driven backend selection**: Workload class → policy file → prioritized backend list. The daemon probes backends at startup and selects the first available. Never hardcode backend preference in application logic.
- **Lifecycle state machine**: 13 states. The main branches are Pending →
  Creating → Running, Running ↔ Paused → Checkpointed, and
  Running → Restoring → Running for checkpoint restore. Hibernation follows
  Running → Hibernating → Hibernated → Resuming → Running; compensation can
  return Hibernating to Running or Resuming to Hibernated. Any non-terminal
  state can enter Destroyed; incomplete cleanup enters RecoveryRequired. State transitions are enforced by
  `blaze_core::lifecycle`. Do not bypass via direct field mutation.
- **MockSpawner fallback**: When the configured backend binary is missing or fails `probe()`, the daemon auto-downgrades to `MockSpawner` with a warning. This keeps API/integration tests functional without a real backend.

## Adding a New Backend

1. Add a variant to `BackendKind` in `blaze-core/src/backend.rs`
2. Implement `BackendSpawner` in `blazed/src/spawner.rs`
3. Register the new spawner in `daemon::build_spawner()` priority logic
4. Add a corresponding `[backends.<name>]` section in config schema (`blaze-core/src/config.rs`)
5. Add policy support: allow the new backend kind in policy `backends` priority lists
6. Add unit tests for `probe()` and `spawn()` (use mock paths for CI)

## Configuration

Runtime config: `/etc/anolisa/blaze/config.toml` + `/etc/anolisa/blaze/policies/*.toml`

Development config: `src/blaze/examples/config.toml` + `src/blaze/examples/policies/`

When modifying config schema, update both the Rust struct in `config.rs` and the example files.

## Commit Scope

Use scope `blaze` for all changes under `src/blaze/`. Examples:

```
feat(blaze): add snapshot backend
fix(blaze): handle missing rootfs gracefully
```

## Verification

Before committing:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps   # ensure no broken intra-doc links
```
