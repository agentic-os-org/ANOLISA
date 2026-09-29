# cosh-shell Source Heavy-Test Inventory

> Tracked risk for in-`src` unit tests that spawn subprocesses, open PTYs, or
> write mock provider scripts — patterns that belong in the integration test
> layers (`raw_cli` / `shell_host` / `logic` / `protocol`) rather than in
> `crates/cosh-shell/src`. Enforced by
> `crates/cosh-shell/scripts/check-layout.sh`.

Each entry records the current count of heavy-test markers a file carries. The
audit fails if a file's count grows beyond its registered value or a new
unregistered file appears, so new spawn/PTY tests must go to the correct
integration target instead of `src`.

## Rules

- Only risks that already exist on `main`/`HEAD` may be registered; new
  spawn/PTY tests added by a feature must live in the appropriate integration
  target. The automatic-compaction feature followed this: its in-`src` tests in
  `slash/session/compact/tests.rs` are process-free state-machine tests, and
  the real SIGTERM/SIGKILL/reap/shell-exit lifecycle checks live in
  `tests/raw_cli/compaction.rs`.
- The audit scans two layers: the named heavy-test markers across all of
  `src`, plus every process-spawn primitive (`Command::new`,
  `process::Command`, `CommandExt`, `fork`/`posix_spawn`/`pre_exec`) inside
  test-scoped code — dedicated test files and inline `#[cfg(test)]` module
  regions — so a spawn cannot bypass the check by picking an unlisted binary.
- The `Count` column must be greater than or equal to the file's current marker
  count; lower it only when the file is actually reduced.

## Registered files

| Count | File | Owner | Reason / migration plan |
|------:|------|-------|-------------------------|
| 8 | `crates/cosh-shell/src/hooks/engine/tests/external.rs` | hooks | Pre-existing external-hook subprocess tests. Migrate to a hook integration target. |
| 7 | `crates/cosh-shell/src/shell_host/adapter.rs` | shell_host | Pre-existing inline `#[cfg(test)]` shell-spawn construction tests (`Command::new("bash"/"zsh")`, never spawned). Migrate to the `shell_host` target. |
| 6 | `crates/cosh-shell/src/hooks/engine/tests/parser.rs` | hooks | Pre-existing hook-config temp-dir tests. Migrate to a hook integration target. |
| 5 | `crates/cosh-shell/src/hooks/engine/tests/project.rs` | hooks | Pre-existing project-trust temp-dir tests. Migrate to a hook integration target. |
| 2 | `crates/cosh-shell/src/hooks/engine/tests/loader.rs` | hooks | Pre-existing loader temp-dir tests. Migrate to a hook integration target. |
| 1 | `crates/cosh-shell/src/recommendation/personal_process_tests.rs` | recommendation | Pre-existing `ps` probe in the personal-process unit tests. Migrate to a recommendation integration target. |
| 1 | `crates/cosh-shell/src/shell_host/raw_runner.rs` | shell_host | Pre-existing `openpty` unit test. Migrate to the `shell_host` target. |
| 5 | `crates/cosh-shell/src/adapter/cosh_core_service/process/tests.rs` | cosh_core_service | Internal `pub(crate)` persistent cosh-core runtime lifecycle tests. The runtime handle is not exported, so the integration targets cannot exercise these paths. |
| 1 | `crates/cosh-shell/src/shell_host/bootstrap.rs` | shell_host | Pre-existing `openpty` unit test. Migrate to the `shell_host` target. |
