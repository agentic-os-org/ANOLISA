# Bounded command execution

[中文版](bounded-execution_zh.md)

`aw-exec` runs one command with byte limits, an absolute deadline and cancellation.
It provides the process transport shared by native hook commands and the
[local Provider Host](provider-host.md). It does not depend on `aw-config`,
`aw-core` or `aw-provider`.

## API and ownership

`run(&CommandSpec, input, Limits, deadline, &AtomicBool)` is synchronous. The
caller supplies literal arguments, a working directory and the complete child
environment. No shell is inserted; shell syntax requires an explicit command
such as `/bin/sh -c ...`. The caller selects executable lookup and passes only
the environment variables the command needs.

Each invocation owns a new process group and three pipes. Input is closed after
the last byte is written. stdout and stderr remain separate byte arrays, including
non-UTF-8 data. The result preserves the native `ExitStatus`, including nonzero
exits and signal termination. A command that closes stdin early may still return
normally; `input_bytes_written` reports pipe writes, not consumption by the child.

The library does not parse JSON, interpret policy decisions or log payloads.
Native adapters interpret command results using the host's protocol. A Provider
Host must separately validate its request/response binding, configuration and
admitted effects. A transport result alone is not evidence of host adoption.

## Execution and cleanup limits

| Boundary | Behavior |
| --- | --- |
| Input limit | Reject before spawning when input exceeds its byte ceiling |
| Output limits | Check stdout and stderr independently; exceeding either stops the group and returns a typed error |
| Deadline | Use the caller's absolute `Instant`; repeated invocations can share the same deadline without resetting a budget |
| Cancellation | Check before spawn and during pipe exchange; the caller owns the atomic flag and any signal handling |
| Early stdin closure | Preserve native output/status and report how many bytes were written |
| Inherited output pipes | Wait for EOF within the execution deadline; an exited leader does not make an open descendant pipe complete |
| Cleanup | Kill the owned group, verify that no member is running and reap the leader; allow a separate fixed one-second cleanup budget |
| Cleanup failure | Return `Error::Cleanup` instead of an unverified result or the original transport error |

The Linux implementation uses nonblocking pipes and bounded reads/writes per
iteration so pipe backpressure or continuous output cannot monopolize the loop.
It observes leader exit with `waitid(WNOWAIT)`, reserving the PID until the last
group signal. Cleanup inspects `/proc` group members and their threads before
reaping the leader. A zombie thread-group leader can still have live threads.
Verification has both a time limit and a scan-entry limit; inaccessible process
metadata causes an explicit cleanup error.

The execution deadline also accounts for time spent spawning, but synchronous
OS calls and kernel-stuck processes do not have a hard realtime bound. The
caller must not reap this library's children or configure automatic SIGCHLD
reaping. Error PIDs are diagnostic identifiers and must not be used for later
signals after the call returns. On unwinding, Drop attempts a group kill and a
nonblocking reap. Linux also arms `PR_SET_PDEATHSIG(SIGKILL)` before exec and
checks for parent death during setup, so a nested transport does not leave its
immediate command running when its owning thread dies. Privilege-changing execs
can clear this signal. Forced termination still prevents verified group cleanup
and does not extend the signal to arbitrary descendants.

Normal completion also terminates remaining members of the command group. This
transport is for commands whose children share the invocation's bounded lifetime;
it is not a service launcher. The bounded pipe API does not change global signal
handlers, create helper threads or reap unrelated children.

`run_foreground` separately runs an interactive Agent with inherited standard
streams and the same owned process-group cleanup. It transfers a terminal only
when the caller owns its foreground, restores that ownership on return, and
forwards signals supplied by the caller. An interactive session has no command
time limit; cancellation starts bounded shutdown. This API does not replace the
separate daemon lifecycle owner.

`run_foreground_observed` adds a one-shot callback after successful spawn and
terminal handoff. It reports the owned leader PID (also its process-group ID)
so callers can bind startup evidence to the actual Agent. The callback must
return promptly and must not reap the child; executor cleanup ownership is unchanged.

## Native semantics and future enforcement

The current delivery serves `tool.before` and `tool.after`. Scheduling remains
with the native adapter: concurrent calls remain independent, sequential hooks
remain sequential, and the library does not manufacture after events or approvals.
`aw-host` composes this transport with structured Provider and native command
execution. The service and Qoder launcher use it without moving native scheduling
into the executor. The other three framework adapters remain subsequent work.
This library does not parse `aw.yaml` or select Core profiles.

The extension boundary separates transport, policy evaluation and effect
application. Adapters report the actual native capabilities; a later OS backend
can supply separately verified enforcement capabilities. Process-group cleanup
is not a sandbox: children can escape via a new session or process group, and a
killed callback can bypass this cleanup. This crate cannot claim final/protected
execution or act as the OS enforcement fallback. Existing Core guarantees remain
unchanged; there is no placeholder backend reporting success.

Execution currently requires Linux. Other platforms return `UnsupportedPlatform`.

## Development validation

Run the focused tests from `src/aw` on Linux with Python 3 installed:

```bash
cargo test --locked -p aw-exec
```

Tests use local fixture commands without Agent logins or model calls. They cover
byte transport, literal arguments, explicit environment/cwd, stream ceilings,
deadlines, cancellation, native exit status, descendants and concurrent calls.
The workspace gate includes this crate's tests, Clippy and rustdoc:

```bash
python3 src/aw/scripts/check.py
```

Run the workspace gate from the repository root. Tests of command transport do
not replace real Agent acceptance of hook results.
