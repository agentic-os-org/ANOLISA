# Policy crash consistency tests

[中文版](README_zh.md)

This suite checks Policy Engine recovery after an abrupt daemon process exit.
It runs the real CLI, UDS server, discovery, reconciliation runtime, and SQLite
repository against a controlled AgentSight HTTP mock. Complementary Rust tests
cover committed repository/reconciler boundaries and, with the optional
`fault-injection` feature, uncommitted SQL transaction boundaries. Ordinary
product builds exclude the fail-rs dependency and instrumentation.

## Goals and evidence boundaries

| Goal | Why it matters | Evidence |
| --- | --- | --- |
| Recover accepted assignments without another user request | An in-memory notification or discovery worker disappears on crash; committed Scope state must remain authoritative. | Discovery recovery and `scope_saved`/`binding_saved` windows |
| Preserve assignment identity and policy snapshots | Updating the current PolicyTemplate must not change an existing Scope's selected revision. | Black-box two-crash Scope scenario; repository reopen contracts |
| Record cleanup responsibility before remote I/O | A remote Apply may succeed while its response or the local result is lost. `UNKNOWN` means cleanup may still be necessary. | `unknown_saved`, `applied_before_result`, and Apply/delete compound windows |
| Preserve irreversible deletion despite an older Apply result | A stale result changes neither status nor deployments; previously registered UNKNOWN responsibility remains. | Three compound windows; CAS contracts |
| Retain Scope until discovery stops and its last Binding is removed | Early parent deletion can lose cleanup responsibility; an Active Scope with no matches must remain available for discovery. | Partial cleanup across two crashes; repository lifecycle contracts |
| Resume safely after another crash during recovery | Restart itself is not an atomic operation. | Two-crash black-box scenarios and `recovery_saved` |
| Preserve SQLite's cross-process WAL locks | An external SQLite reader must not corrupt SHM coordination or cause SIGBUS. | WAL lock contract and black-box `saved()` queries using independent read-only connections |

The fault model is **SIGKILL of AgentSecCore**, with the same SQLite files and
host boot retained. The HTTP mock and selected target processes survive unless a
case explicitly terminates a target. This is not power-loss, host-reboot,
filesystem-corruption, AgentSight-restart, or real eBPF enforcement coverage.
Authentication is also mocked: the HTTP server checks for a Bearer header, not
token validity.

Recovery does not promise exactly-once HTTP delivery. An uncertain Apply may be
replayed against the same target identity. The tests specifically prohibit an
extra Apply for a persisted `Ready` Binding, and prohibit resuming Apply after
deletion has superseded it.

## Daemon black-box matrix

The following lifecycle tests and the storage cases below are default CI coverage,
except for the opt-in reproducer for the deferred protocol gap. Kill windows are observed
through RPC, committed SQLite state, and mock HTTP gates; no product failpoint
selects an exact instruction inside the daemon.

| Test | Confirmed setup and crash window | Expected recovery | Current outcome |
| --- | --- | --- | --- |
| [`test_scope_recovers_discovery_and_preserves_binding_across_two_crashes`](test_scope_recovery.py) | Scope references policy revision 1, no matching process exists, and current policy is updated to revision 2. Kill; start a matching process; recover to `Ready`; kill again. | Discovery creates one Binding using the original policy snapshot. Second restart preserves the same Binding view and sends no additional POST. Explicit final deletion removes local and remote state. | Pass |
| [`test_process_exit_during_crash_cleans_binding_without_repinning_scope`](test_scope_recovery.py) | PID pin and `Applying` Binding are committed; mock health response is held before Apply POST. Kill daemon, then terminate target process while daemon is down. | Retire the vanished instance without sending POST; retain the Active Scope and its original PID pin. Final explicit Scope deletion succeeds. | Pass |
| [`test_delete_then_crash_cleans_apply_committed_before_its_reply`](test_binding_recovery.py) | Remote Apply has committed but its HTTP response is held. Scope deletion commits `Deleting`/`PendingDelete`, with Deployment still `UNKNOWN`; kill daemon. | Recover deletion, clean the recorded target, remove Binding and Scope, and send no additional POST. | Pass |
| [`test_delete_during_apply_then_crash_cleans_committed_target`](test_binding_recovery.py) | Hold Apply before coordinator execution. Accept Scope deletion, then release Apply into an `after_apply` gate before killing the daemon. Its HTTP reply remains blocked. | Recovery deletes the committed target with HTTP 204, removes Binding and Scope, and sends no additional POST. No pre-crash DELETE or recovery 404 is allowed. | Pass |
| [`test_late_apply_cannot_outlive_confirmed_scope_cleanup`](test_binding_recovery.py) | Received Apply is held before coordinator execution. Accept deletion, kill and restart daemon. Recovery DELETE gets `binding_not_found`; local cleanup finishes; release the old Apply. | An old Apply must not leave a remote target after confirmed local cleanup. | **Opt-in known failure: deferred AgentSecCore/AgentSight protocol gap** |
| [`test_partial_scope_cleanup_survives_two_crashes_without_reapplying`](test_binding_recovery.py) | Two Bindings are `Ready`. Hold both committed DELETE responses, kill daemon; restart and release one target's replies. Observe one remaining Binding and `Deleting` Scope, then kill again. | Second recovery cleans the remaining target and removes Scope. Exactly two original POSTs; no Apply during cleanup recovery. | Pass |

### Storage admission and retry recovery

[`test_storage_recovery.py`](test_storage_recovery.py) adds real-process checks:

| Test | Required behavior |
| --- | --- |
| `test_unusable_policy_database_rejects_startup_without_losing_responsibility` | Unknown schema, malformed Binding JSON, unsafe permissions and a real SQLite writer lock reject startup before UDS or remote I/O. Preserve all SQL data, schema version and file identity; ordinary WAL checkpointing may change main-file bytes. |
| `test_shared_sqlite_write_lock_retains_both_results_and_recovers_without_http_replay` | Hold an external `BEGIN IMMEDIATE` after two remote Applies commit. Reject new PAP writes, retain both UNKNOWN results, then reach Ready without repeating HTTP after releasing the lock. Reads return committed data or explicit unavailable, never a false empty list. |
| `test_pending_retry_budget_resets_in_a_new_daemon_process` | After two failed attempts and SIGKILL, the same DB/Binding can use a fresh five-attempt budget: four failures followed by success. |
| `test_failed_binding_survives_restart_until_explicit_scope_retry` | Independent work completes after restart while the original Failed Binding stays terminal; explicit Scope retry resumes it. |

BUSY is exercised through real SQLite locking. Real READONLY/FULL classification
and Runtime fault injection remain separate component checks, not filesystem
failure experiments. Ordinary-UID authorization is covered by
[`test_policy_cli_errors_e2e.py`](../e2e/test_policy_cli_errors_e2e.py); its positive
and denial cases require root with permission to switch client UID. A namespace
mapping only UID 0 cannot run those cases.

### HTTP gates and repeated runs

[`conftest.py`](conftest.py) exposes four boundaries:

| Gate | What has happened when it is reached |
| --- | --- |
| `health` | Health request arrived; response is delayed before the daemon can prepare/send Apply. |
| `before_apply` | Apply POST arrived; coordinator work has not executed. |
| `after_apply` | Remote Apply committed and coordinator lock was released; response is delayed. |
| `after_delete` | Remote deletion committed and coordinator lock was released; response is delayed. |

The delete-during-Apply case arms both Apply gates before creating the Scope.
It accepts deletion before remote Apply commits, then confirms the commit before
killing the daemon. This controls ordering without sleeps and does not claim to
cover an Apply that arrives after recovery cleanup.

The two-target case arms both DELETE gates before requesting deletion. Each gate
holds repeated responses for its selected target, including requests from a
restarted daemon. This preserves the partial-cleanup window without assuming
request order or blocking unrelated coordinator operations.

Gates establish meaningful windows, but worker scheduling, discovery, HTTP
handling, and concurrent SQLite readers still race. Repeating the suite helps
detect those races; repetitions are not automatic retries that erase failures.

## Precise SQLite/PCP crash matrix

[`crash_recovery.rs`](../../../v2/crates/asc-policy-repository-sqlite/tests/crash_recovery.rs)
kills a child process using the real repository and `BindingReconciler`, with a
test Adapter/Client and an independently surviving TCP mock ledger. Its wrapper
emits a gate after a selected commit or external operation; the parent confirms
the gate, then sends SIGKILL. Discovery synchronization and recovery are driven
by the test harness, not the daemon startup/runtime wiring.

For creation windows, recovery must reach one `Ready` Binding with the original
policy and, if already created, the original Binding ID. Final deletion must
leave no Binding, Scope, or mock target. For deletion windows, recovery must
complete the accepted deletion.

| Window | Boundary before SIGKILL | Reason for selecting it |
| --- | --- | --- |
| `scope_saved` | Active Scope committed; no Binding yet. | Assignment must survive loss of discovery startup/notification. |
| `binding_saved` | `PendingApply` Binding committed; no claim yet. | A committed Binding must not depend on an in-memory queue entry. |
| `claim_saved` | `Applying` claim committed; no target registration yet. | A crashed execution owner must not strand the Binding. |
| `unknown_saved` | Target identity and `UNKNOWN` committed before Apply I/O. | Cleanup responsibility must survive even without a confirmed result. |
| `applied_before_result` | Mock Apply committed; local Deployment remains `UNKNOWN`. | Cover remote success with a lost local observation. |
| `result_saved` | `Ready` result committed. | Restart must not reapply already completed work. |
| `delete_intent` | Scope `Deleting` committed; discovery stop barrier not finished. | Recovery must finish the barrier and propagate deletion to owned Bindings. |
| `stop_saved` | Discovery stop marker and Binding deletion intent committed. | Accepted deletion must not depend on the subsequent wakeup. |
| `delete_unknown_saved` | Delete target registration committed as `UNKNOWN`; DELETE I/O not started. | Cleanup remains necessary across loss of the deleting worker. |
| `removed_before_result` | Mock DELETE committed; local deletion result not saved. | Repeating cleanup after a lost result must be safe. |
| `delete_result_saved` | Binding removal and final Scope removal committed. | Completed cleanup must stay complete on restart. |
| `recovery_saved` | After an earlier `claim_saved` crash, recovery commits `PendingApply`; kill again. | Recovery's own state transition must survive another crash. |

Three additional windows combine **an unfinished Apply with accepted Scope
deletion**, rather than testing creation and deletion only in isolation:

| Window | Persisted state at crash | Required recovery |
| --- | --- | --- |
| `compound_delete_intent` | Remote Apply committed; Scope `Deleting`; Binding still `Applying`; Deployment `UNKNOWN`. | Finish discovery stop and clean the target. |
| `compound_pending_delete` | Remote Apply committed; Scope `Deleting`; Binding `PendingDelete`; Deployment `UNKNOWN`. | Continue deletion without another Apply. |
| `compound_result_superseded` | Old Apply result rejected entirely; Binding remains `PendingDelete` and Deployment remains `UNKNOWN`. | Clean the registered target without another Apply. |

Each compound case additionally asserts the remote operation sequence is exactly
`apply, delete`. These wrapper scenarios use two parent tests covering
**12 + 3 windows**. Rust also collects the `crash_child` helper and the SQL cases
below, so the test count does not equal the number of crash windows.

The unit test
[`sigkill_before_discovery_commit_rolls_back_pin_and_all_bindings`](../../../v2/crates/asc-policy-repository-sqlite/src/tests.rs)
also kills a child before the production discovery transaction commits. It checks
that neither a PID pin/Binding pair nor a 200-instance batch partially survives,
then admits the same instances after reopening. Its gate is compiled only under
`cfg(test)` and is absent from the daemon binary.

### Optional white-box transaction matrix

[`fault_points.rs`](../../../v2/crates/asc-policy-repository-sqlite/tests/crash_recovery/fault_points.rs)
provides six paired scenarios covering ten transaction windows. Each scenario
has a separately named `baseline_*` test that runs the same preparation and
repository operation without configuring a failpoint or killing its child.
Baselines run with default features and also with `fault-injection` enabled;
the six crash variants require that feature. Both modes check their expected
transaction state and share replay, convergence and cleanup assertions.

The crash variants reuse the existing child process, SIGKILL, real
repository/reconciler, and remote ledger. Only the child configures fail-rs
callbacks: a callback confirms
its boundary and parks while the parent kills the process. Panic, injected
errors, and normal transaction rollback do not substitute for process death.

Scope admission and instance synchronization are separate transactions. A
committed Scope with no Bindings is valid; the pin and Binding changes within
an individual synchronization transaction must commit together.

| Window | Boundary before SIGKILL | Recovery requirement |
| --- | --- | --- |
| `sql_scope_inserted` | Scope inserted before admission commits. | No Scope or Binding survives; the earlier Policy commit remains. Recovery does not discover an unaccepted assignment. |
| `sql_instances_pinned` | PID pin updated before Binding insertion. | Pin and Binding expansion roll back; the committed Active Scope remains. |
| `sql_instances_binding_inserted` | First of two policy Bindings inserted. | No partial expansion survives. Recovery creates exactly two Ready Bindings; another restart preserves their identities without Apply. |
| `sql_discovery_stopped` | Stop flag updated before Binding retirement. | Flag and retirement roll back; the previously committed Scope deletion survives and completes on recovery. |
| `sql_discovery_binding_retired` | First of two Bindings retired. | Both original Binding snapshots and versions remain, stop flag is unset, and recovery deletes both without Apply. |
| `sql_cas_state_saved` | Binding state saved before write receipt SQL. | State, version, Deployment, and receipt all roll back. Replaying the original write applies once. |
| `sql_cas_after_transaction` | CAS committed before returning its result. | State and matching digest/receipt survive. Replaying the same write returns `AlreadyApplied` without another version increment. |
| `sql_unknown_state_saved` | UNKNOWN written before its transaction commits; Apply claim already committed. | Applying survives without Deployment or remote Apply. Recovery completes one Apply; a second restart does not repeat it. |
| `sql_binding_deleted` | Last Binding deleted before Scope finalization SQL. | Binding and Deleting Scope both survive rollback, including UNKNOWN cleanup responsibility. Recovery may repeat Delete. |
| `sql_scope_finalized` | Last Binding and Scope deleted before transaction commit. | Both survive rollback with their cleanup responsibility; recovery removes both and the remote target. |

Every reopened database is checked with `quick_check` and `foreign_key_check`.
Failure retains the temporary directory and prints its path, including the DB,
WAL/SHM files if present, and remote ledger. Callbacks inside transactions never
reenter the repository. These tests do not inject faults inside SQLite's WAL
write/fsync implementation or cover power loss.

Final cleanup removes the Binding and finalizes its Scope in one transaction.
There is no earlier committed empty-Deployment snapshot. Both removal windows
therefore assert a Deleting Binding at status version 5 with one UNKNOWN
Deployment and `lastConfirmed=PRESENT`, even though the remote target is already
absent. Recovery repeats Delete, removes Binding and Scope together, and a second
restart performs no further I/O. This tests retained cleanup responsibility
rather than assuming exactly-once remote deletion.

Parent scenarios share a standard-library mutex for their full lifetime,
including repository setup and teardown. Without this serialization, a child
spawned by another test can inherit a live database lease FD until exec, even
with `CLOEXEC`, causing a false `AlreadyOpen` during immediate reopen. Scenario
serialization preserves the internal Apply/delete interleaving. A failed
scenario drops its resources before releasing the guard; subsequent scenarios
recover the poisoned mutex and still run. No retry or lease relaxation is used.

The complete feature-enabled suite collects fifteen tests: six baselines, six
crash variants covering ten white-box windows, the two existing parents and the
child helper. Baseline failures point to normal-operation, fixture or environment
problems. A passing baseline with a failing crash variant narrows investigation
to interruption, recovery or injection; it does not by itself prove a product bug.
Stale observation writes are now unreachable. Their transaction hook was removed;
`compound_result_superseded` checks rejection retains UNKNOWN
responsibility and deletion intent instead.

## Storage failure and runtime composition regression

[`storage_faults.rs`](../../../v2/apps/asc-daemon/tests/reconciliation/storage_faults.rs)
uses real SQLite, PCP, Adapter and a multi-worker runtime. Its repository wrapper
models pre-write I/O failure or post-commit `OutcomeUnknown`. This is a component
fault test, not SIGKILL or a physical disk fault. Restart cases close the runtime,
release the repository and reopen the same database.

Four write boundaries (claim, UNKNOWN registration, Apply result, final removal),
two commit outcomes and live/reopen recovery give sixteen cases. A blocked Binding
fails six storage attempts while another must reach Ready. Scope deletion sends
no enqueue notification. Recovery must remove its Binding and Scope, retain the
original write ID through retries, avoid another Apply, and repeat DELETE only
when restart loses an uncommitted deletion result.

Four further cases use the default four automatic retries with shortened timers.
Four retryable remote Apply failures precede result-storage failure and Scope
deletion. Cases cover live recovery, reopen, normal notification, and a new Delete
that must retry its first failure. Rejected old results leave UNKNOWN untouched;
the new Delete must receive its own retry budget.

## Related contracts and known gap

- [`contracts.rs`](../../../v2/crates/asc-policy-repository-sqlite/tests/contracts.rs)
  checks CAS/ABA conflicts, old Apply observations versus new deletion, atomic
  Scope finalization, snapshot admission, reopen behavior, WAL lock retention,
  and unsafe DB/sidecar rejection. These are not all SIGKILL tests.
- [`endpoint_recovery.rs`](../../../v2/crates/asc-policy-repository-sqlite/tests/endpoint_recovery.rs)
  verifies persisted cleanup rejects a changed endpoint without sending I/O,
  retains responsibility, and succeeds after restoring the endpoint with a
  rotated token. It uses close/reopen, not process kill.

The protocol gap is **premature release of cleanup responsibility**: AgentSecCore
interprets `binding_not_found` as `Absent`, but AgentSight does not persist a
deletion fence for an unknown ID. An older Apply can execute after that DELETE.
Local `UNKNOWN` bookkeeping and CAS cannot impose ordering on remote work
already accepted before a daemon crash. The protocol fix is deferred from the
current PR. Its reproducer retains the original safety assertion and is skipped
unless `ASC_TEST_DEFERRED_PROTOCOL=1`. With that flag it still fails; passing the
default CI scenarios does not establish that this protocol gap is fixed.

Record the tested source version, commands, environment, results and skipped cases
in the corresponding PR/CI. Older test counts and PASS records do not establish
current coverage or crash safety beyond this fault model.

## Running and diagnosing

Use Linux and the project's Python 3.11.6 environment with pytest. Build from
`src/agent-sec-core`, then run daemon tests inside a root test environment:

```bash
(cd v2 && cargo build --locked -p asc-daemon -p asc-cli)
PATH="$PWD/v2/target/debug:$PATH" agent-sec-cli/.venv/bin/python -m pytest tests/v2/crash_consistency -v
```

The mock binds `127.0.0.1:7396`; stop any service using that port in the test
environment. Tests share that address and must run serially. The fixture uses
`/var/log/sysak/.agentsight/.dashboard_token`, preserves an existing token, and
removes a token it created. SQLite, sockets, logs, and copied target executables
are isolated under pytest's temporary directory. Prefer a disposable root
container to an installed system daemon's environment.

To run the five default scenarios with the deferred case deselected, or explicitly
enable the unchanged protocol-gap safety assertion:

```bash
PATH="$PWD/v2/target/debug:$PATH" agent-sec-cli/.venv/bin/python -m pytest tests/v2/crash_consistency -k 'not late_apply' -v
ASC_TEST_DEFERRED_PROTOCOL=1 PATH="$PWD/v2/target/debug:$PATH" agent-sec-cli/.venv/bin/python -m pytest tests/v2/crash_consistency/test_binding_recovery.py -k late_apply -v
```

Repeat the first command as independent runs, recording every outcome. The full
default suite reports the protocol reproducer as skipped. Run the complementary Rust
checks from the same component directory:

```bash
(cd v2 && cargo test --locked -p asc-policy-repository-sqlite)
```

To isolate a baseline failure, run the same SQL scenarios without injection
using default features:

```bash
(cd v2 && cargo test --locked -p asc-policy-repository-sqlite --test crash_recovery baseline_ -- --nocapture)
```

Run the paired baseline and crash tests together with instrumentation. Keep their
build artifacts separate from normal daemon/package artifacts; do not enable
`fault-injection` or use `--all-features` in release builds:

```bash
(cd v2 && cargo test --locked -p asc-policy-repository-sqlite --features fault-injection --test crash_recovery --target-dir target/fault-injection -- --nocapture)
```

The existing Source Build workflow runs both baseline and crash variants with
this command in its `Run V2 policy crash consistency tests` step on Ubuntu 22.04
and Alinux4. Rust reports each variant separately and still runs the other tests
after an assertion failure. Either variant failing fails the suite. The
Python V2 E2E command does not collect these Rust tests, and raw/RPM release
builds leave fault injection disabled.

Set `ASC_CRASH_DAEMON_LOG=debug` for daemon diagnostics; the fixture leaves CLI
stderr behavior unchanged. Teardown saves `daemon-N.stderr.log` and
`daemon-N.stdout.log` under each test's temporary directory and prints daemon
exit codes and HTTP traces in captured output. Correlate Scope/Binding IDs,
target IDs, CAS versions, and OTel trace/span IDs when emitted. SIGKILL may lose
buffered diagnostics; committed SQLite state and mock operation traces determine
which boundary was reached. Diagnose an unexpected exit, barrier timeout, and
recovery assertion separately before changing code or assertions.
