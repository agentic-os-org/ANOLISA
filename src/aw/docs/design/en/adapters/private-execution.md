# Private execution leases

[中文版](../../zh/adapters/private-execution.md)

Applications supply short-lived tool credentials through an explicit execution
factory. AW owns admission, result redaction and cleanup ordering; the application
owns credential acquisition and revocation. Native model/daemon authentication
keeps its own lifecycle. Use ordinary resident launches. Explicit idle updates
apply policy to subsequent tasks after all Queries/replies and private leases
drain; factory changes require a rolling restart.

## Ownership and environment boundaries

| Adapter | Lease boundary | Explicit consumer |
| --- | --- | --- |
| Qoder SDK 1.0.56 / Worker 1.1.78 | One lease per managed MCP handler invocation, owned by its Query | `bridge.executionEnvironment()` at client or subprocess creation |
| QwenPaw 2.2.2.post1 / AgentScope 2.0.10 | One lease per managed Agent reply, shared by that reply's tools | `execution_environment()` in the installed native environment extension |

Both contracts use `aw.private-execution/v1`. Execution helpers return copies of
the admitted environment plus declared secrets. Hook helpers return a separate
public view. Neither helper mutates the process environment. The application
must declare sensitive values in `secrets` and exclude their keys and values
from the Hook view. Do not place credentials in model-visible tool arguments.
The redactor must cover all returned fields, including structured MCP data and
error content, while preserving the native result structure and error status.

The integration applies only where the native tool explicitly consumes the
helper. Qoder's built-in Bash has no private-environment injection boundary in
the pinned SDK. The QwenPaw application must install its native environment
extension before enabling a custom factory. An ordinary shell security Hook
does not install either execution extension.

## Qoder public factory

The managed bridge exports `PRIVATE_EXECUTION_VERSION`, `executionEnvironment`
and `hookEnvironment`. Pass `awExecutionFactory` in `createQuery().options`;
the bridge consumes this option before calling the native SDK. The factory
receives a frozen copy of `{ invocationId, toolName, input }` and `{ signal }`.
It returns a fresh lease for each invocation:

| Lease field | Contract |
| --- | --- |
| `version` | `bridge.PRIVATE_EXECUTION_VERSION` |
| `environment` | Execution environment string map |
| `hookEnvironment` | Public Hook environment string map |
| `secrets` | Optional string map merged into the explicit execution view |
| `redact(value)` | Synchronous redaction of a complete, cloned result |
| `release()` | Asynchronous credential revocation and owned-file cleanup |

An application-owned credential service can implement this factory without
changing the SDK's authentication callbacks:

```javascript
const current = await bridge.createQuery({
  prompt,
  options: {
    daemon,
    cwd: process.cwd(),
    settingSources: [],
    awExecutionFactory: async (invocation, { signal }) => {
      const lease = await acquireToolLease(invocation, { signal });
      return {
        version: bridge.PRIVATE_EXECUTION_VERSION,
        environment: lease.environment,
        hookEnvironment: lease.publicEnvironment,
        secrets: lease.secrets,
        redact: (value) => lease.redact(value),
        release: () => lease.revoke(),
      };
    },
  },
});
```

`acquireToolLease` is application code. It must clean up partial acquisition
when it throws. A handler registered through `bridge.createSdkMcpServer()`
passes `bridge.executionEnvironment()` explicitly to its subprocess or client.
`bridge.hookEnvironment()` exposes only the admitted public view inside that
operation. Native schemas, permissions and approved arguments retain their
existing checks; see [Structured MCP dispatch](structured-mcp.md).

The Query owns the original factory and handler promises through their `finally`
blocks. The handler must return a complete result; streaming iterators are
rejected. AW clones and redacts that result, awaits `release()`, then allows the
Post checks and native delivery. Cancellation waits for the original operation,
including a factory that finishes after cancellation, before releasing a lease.
Applications must honor the supplied signal and complete their finalizers within
the Query's shared cleanup deadline.

## QwenPaw reply factory

The existing middleware context accepts `aw_execution_factory`, an asynchronous
context-manager factory called once per reply, and `aw_environment_version: 1`
to declare the installed native execution extension. Its yielded
`PrivateExecution` object contains `version`, `environment`, `hook_environment`,
`secrets` and a synchronous, idempotent `redact` callable. The factory's `finally`
block revokes credentials and removes its owned files.

Tools consume `execution_environment()` from the active reply context. Hooks
use `hook_environment()`; the task scope is opened with this public view.
Streaming output is held until a complete response can be redacted, including
secrets split between chunks. Authorization-bearing arguments must remain
unchanged. Native tools, generators and callback finalizers finish before cleanup.
QwenPaw attempts task-scope closure, then exits the credential factory. Reply
cleanup stays pending until that finalizer settles; lease failure fences new
requests process-wide. This ownership includes offloaded tools and their
completion callbacks.

## Failure and lifecycle

In Qoder, an exception from the private factory, handler or redactor stops the
entire owning Query, including its subsequent tool dispatch. AW reports
`AW private execution failed; tool result withheld`; the original exception may
contain credentials and is not forwarded to Hooks or the model. A returned MCP
error envelope (`isError: true`) follows the normal redaction and Post checks;
it follows the tool-result contract. After native drain and confirmed revocation,
the Qoder scope retires and applications can create a fresh Query. Native
cleanup failure retains ownership; lease cleanup failure or timeout also fences
new work process-wide until worker restart. Qoder shares 60 seconds across native
shutdown, lease cleanup and callback drain; QwenPaw shares ten seconds across
shutdown and nested cleanup. A cancelled await alone
does not prove that the original operation has finished.

Ordinary resident Qoder and QwenPaw admit `tool.after` steps with
`on_error: withhold_result`. A required Post failure or timeout prevents result
delivery and subsequent dispatch. Qoder closes the native Query before replying
to the failed callback or MCP request, then drains original callbacks before
scope retirement. Tool side effects that happened before Post remain in effect.
`on_error: report` keeps the configured reporting behavior. Qoder result
replacement remains unsupported.

`--reloadable` uses a separate idle-update contract without strict Post or
private execution leases. Qoder rejects `awExecutionFactory` in that mode.
Keep native permissions and the caller-owned daemon start/stop lifecycle.
Qoder requires `settingSources: []` and rejects settings,
unadmitted plugins, extra CLI flags and runtime executable/transport overrides. Business
factories depending on those inputs need an integration that satisfies the
runtime admission contract before deployment. The public factory above provides
the credential seam without changing those guards.

See the [shared security configuration](../../../../crates/aw-service/examples/aw.sdk-security.yaml)
and [native integration guide](../../../../../../docs/developer-guide/en/aw/deployment/resident.md)
for launch commands. The implementation contracts live in
[Qoder private execution](../../../../crates/aw-adapter/native/qoder-sdk/private.mjs)
and [QwenPaw private execution](../../../../crates/aw-adapter/native/qwenpaw/aw_private.py).

## Native Bash consumers

`awNativeExecutionFactory` owns one lease per native Bash invocation, separately
from managed MCP's `awExecutionFactory`. Its argument and lease API is defined in
the [Qoder SDK guide](../../../../../../docs/developer-guide/en/aw/agents/qoder-sdk.md#native-bash-command-consumers).
The Adapter preserves these ordering invariants:

1. Security inspects frozen logical input; native rules evaluate that same input.
   Only a subsequent explicit `canUseTool` allowance publishes executable wrapper
   arguments. The pinned Worker can skip Hook asks in bypass/auto, so admission,
   mode changes and the effective Bash Hook mode must reject those paths.
2. Publication records ownership before the permission response is sent. An
   uncertain response cannot prove that a ticket is unused. Native rejection
   before publication releases its unused lease without consumer settlement.
3. Consumer acknowledgement precedes finite-input EOF and native shutdown.
   Original callback drain precedes final revocation. Timeout-backgrounding,
   result messages and background snapshots alone do not prove consumer completion;
   typed session/tool/task identities correlate each acknowledged consumer.
4. Normal settlement has no task-length cap. Cancellation uses the existing
   shared 60-second budget for consumer join, native close, callback drain and
   lease cleanup. Unconfirmed cleanup retains leases, snapshots and scope
   ownership and fences new work until worker restart.

## Execution-asset verification

Every Query opens admitted files without following symlinks. Up to 64 successful
checks are cached by path, expected digest and `(dev, ino, size, mtimeNs, ctimeNs)`;
changed identity or digest requires a rehash. Recent one-second or whole-second
ctime does not establish a reusable generation. Streaming reads enforce the
actual byte limit and recheck metadata after hashing. Cache eviction only causes
rehashing. Snapshot traversal anchors no-follow descriptors to opened directories
and counts actual entries and bytes. Resource limits and accepted plugin fields
are defined in the [configuration reference](../../../../../../docs/developer-guide/en/aw/configuration.md#qoder-sdk-execution-assets).
