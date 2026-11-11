# cosh-shell Source Heavy-Test Inventory

> Tracked risk for in-`src` unit tests that spawn subprocesses, open PTYs, or
> write mock provider scripts — patterns that normally belong in the integration
> test layers (`raw_cli` / `shell_host` / `logic` / `protocol`) rather than in
> `crates/cosh-shell/src`. Enforced by
> `crates/cosh-shell/scripts/check-layout.sh`.

Each entry records the current count of heavy-test markers a file carries. The
audit fails if a file's count grows beyond its registered value or a new
unregistered file appears, so new spawn/PTY tests must go to the correct
integration target instead of `src`.

## Rules

- Only risks that already exist on `main`/`HEAD` may be registered; new
  spawn/PTY tests added by a feature must live in the appropriate integration
  target, except for the individually recorded, explicit user exception below.
  The automatic-compaction feature followed the general rule: its in-`src` tests in
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
- Counts are scanner markers, including imports and command construction, not
  the number of tests. Registration is a per-file ceiling, so a mixed file must
  not be registered until its unapproved cases have moved out.

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

## Explicit user exception: bounded private components, 2026-11-06

The user explicitly approved retaining bounded private deadline, reap, FD,
lock, and protocol tests in `src` when an integration target cannot reach the
necessary private state without expanding the library's public API. This is a
case-by-case exception for the entries below, not a relaxation of the general
rule. Actual Cosh executable and Bash/PTY interaction scenarios still belong in
integration targets. A PTY used only for a private FD flag/inode/lifetime
assertion with a controlled fault helper can qualify; the syscall alone does
not make it a Bash scenario. No production-source `#[path]` harness, scanner
change, or new public API is authorized by this registration.

The counts below were checked against the 2026-11-06 layout report and current
source. A test-harness executable invoked with an exact private helper test is
identified explicitly; it is not the product Cosh executable. The shell helpers
in the registered lifecycle cases are synthetic children controlled by the
private deadline/reaper, not Bash or interactive-shell acceptance tests.

| Count | File | Owner | Reason / migration plan |
|------:|------|-------|-------------------------|
| 12 | `crates/cosh-shell/src/diagnostics/health/runtime/job/tests.rs` | diagnostics / process (P2) | Approved private deadline/reap cases: cold exec, inherited alarm state, cancellation, leader exit, and descendant cleanup. Synthetic children and the exact `deadline_job_child` test-harness helper use `JobDeadline` and the registered child owner; readiness/exit loops have finite limits. Assertions require private `CommandChild`, slot ownership, and worker command state; exporting those only for integration tests would expand the API. No actual product worker or PTY case is registered. |
| 10 | `crates/cosh-shell/src/process/worker/tests.rs` | process (P2) | Approved bounded private protocol/cleanup cases: frame limits, receipt before EOF, confirmed cleanup before publication, and child-only syscall fault injection covering FD isolation and exec-error propagation. The supervised synthetic children have one/two-second deadlines and finite reap checks. The tests inspect private `BoundedWorker` fields and `ReapOwner`; retain them here rather than expose those internals publicly. |
| 10 | `crates/cosh-shell/src/diagnostics/health/collectors/command/tests.rs` | diagnostics / process (P2) | Approved private reap and controlling-terminal FD isolation cases. Two markers construct detached synthetic children for private owner/reap assertions. Three belong to `parent_controlling_terminal_does_not_reach_command`: a three-second registered exact test-harness helper gets a controlling terminal, then runs a bounded `sh` `/dev/tty` probe to prove setsid removes terminal access. The helper is not a product Cosh executable or a Bash user/PTY interaction. The assertions need private child ownership and command isolation internals, so no public test API is added. |
| 3 | `crates/cosh-shell/src/runtime/login/invitation/tests.rs` | login invitation (P5) | Approved private cancellation/ownership cases. Synthetic `sleep 30` children are always launched through `Preparation::fixture` with the real 250ms `BoundedWorker` deadline and cancelled/dropped with state. The assertions require private pending preparation and prompt/session state; they do not run an actual Cosh invitation binary or terminal. |
| 4 | `crates/cosh-shell/src/runtime/shell_integration/tests.rs` | shell integration (P7) | Approved private FD lifetime case `killed_tty_writer_preserves_the_parent_terminal_file_status_flags`. A test-harness helper runs only private `write_frame` with a five-second deadline on a deliberately full terminal or pipe FD; a one-second readiness check precedes SIGKILL/reap, and the parent verifies its shared O_NONBLOCK flags remain unchanged. No Bash or product Cosh executable runs. Keeping the private writer directly reachable avoids exporting an FD writer for tests. The actual unsupervised product-worker flag rejection was moved to `tests/raw_cli/passthrough.rs`. |
| 3 | `crates/cosh-shell/src/config/guide/tests.rs` | guide state (P4) | Approved bounded private lock/protocol cases. Four exact test-harness claim helpers share a three-second deadline, reserve child-owner slots before spawn, and use isolated groups plus poll/cleanup instead of unbounded wait. A stalled-helper regression shortens the parent deadline to 50ms and confirms targeted reap within two seconds. The assertions need private GuideStore/RuntimeState claim state; no public state-store API or production lock retry/timeout is added. |
| 2 | `crates/cosh-shell/src/diagnostics/health/runtime/scan/tests.rs` | diagnostics (P2) | Approved private protocol/sealing case `unconfirmed_process_cleanup_cannot_project_a_healthy_scan`. It inserts a registered synthetic child with an expired cleanup deadline and checks the sealed report through private `ScanState::Running`; no second report API or public process injection hook is introduced. |
| 3 | `crates/cosh-shell/src/runtime/login/editor/tests.rs` | login editor (P4/P5) | Approved private FD/control and Store lease cases. The fixture command is construction only. The exact test-harness child for `store_lease_survives_parent_release_until_the_editor_child_exits` uses a three-second JobDeadline and ReapOwner, verifies the inherited same-OFD lock survives parent FD release, and calls private transact(Restore) to require LockBusy while the child owns it. No actual account, vipw, Bash, or product Cosh executable runs; actual orphan-vipw/SIGKILL qualification remains in integration. |
| 1 | `crates/cosh-shell/src/insight/guidance/quote.rs` | guidance (P7) | Approved noninteractive private argv-quoting protocol case. Fixed control-character inputs are serialized as one Bash argument; the child has null stdin, removed BASH_ENV/ENV, a one-second try_wait deadline with kill/wait cleanup, and bounded 1025-byte output reads. It is not a Bash user/PTY interaction and avoids adding a public quoting API solely for tests. |
| 1 | `crates/cosh-shell/src/runtime/startup/descendants.rs` | startup (P5) | Approved private protocol construction case `supervisor_and_reconstructed_profile_shell_keep_the_internal_marker`. It constructs and parses commands but never spawns Bash, a product helper, or a PTY. The assertion needs private supervisor/spec constructors; no public constructor is added for a process-free check. |

| 1 | `crates/cosh-shell/src/shell_host/bootstrap/tests.rs` | shell_host / startup (P5) | Approved private FD-only case `close_on_exec_sets_fd_flag`: opens a PTY descriptor and checks the private close-on-exec flag setter without spawning a shell or product executable. Actual bootstrap, shell marker, and public banner scenarios were migrated to integration and verified on ALinux4 ARM64 before deleting the obsolete source tests. No public test API is added. |

The package lifecycle shell-spawn cases remain in
`tests/shell_host/login_lifecycle.rs`. Actual bootstrap cases are in
`tests/shell_host/bootstrap.rs`; automatic banner CLI admission is in
`tests/raw_cli/health_banner.rs`. None is registered as a source-test exception.

The exact ignored entries `private_worker_child`, `deadline_job_child` and
`controlling_terminal_child` are supervised subprocess entry points for the
approved private lifecycle tests above, not skipped regression cases. Their
parents invoke them explicitly with deadlines. `check-test-inventory.sh` checks
this exact set separately from the ordinary ignored-test ceiling. They remain
available in both test binaries because the parents use `current_exe`.
Shared renderer assertions run once under the library test owner; this removes
duplicate execution without removing assertions or expanding the production API.
