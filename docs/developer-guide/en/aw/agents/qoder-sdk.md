# Qoder SDK

[中文版](../../../zh/aw/agents/qoder-sdk.md)

## Dependencies and factories

Use `@qoder-ai/qoder-agent-sdk@1.0.56`, bundled international Worker 1.1.78 and Node.js 22+. Install the SDK in the application directory and retain SDK authentication.
Set a target under `spec` in a complete AW configuration.

```yaml
agents:
  sdk:
    adapter: qoder-sdk
    argv: [node, /absolute/application/worker.mjs]
```
Use managed daemon and Query factories, closing both in finally.

```javascript
import { startDaemon, query, accessTokenFromEnv } from "@qoder-ai/qoder-agent-sdk";
import { pathToFileURL } from "node:url";

const prompt = process.argv[2];
if (!prompt) throw new Error("A prompt is required");
const bridge = process.env.AW_QODER_SDK_BRIDGE
  ? await import(pathToFileURL(process.env.AW_QODER_SDK_BRIDGE)) : undefined;
const makeQuery = bridge ? bridge.createQuery : async (request) => query(request);
const makeDaemon = bridge ? bridge.startDaemon : startDaemon;
let daemon, current, stopping = false, cleanup, initialized;
function close() {
  return cleanup ??= (async () => {
    // Creation failures are reported by the main task; still close its daemon.
    await initialized.catch(() => {});
    try { await current?.return(); }
    finally { await daemon?.stop(); }
  })();
}
function stop() {
  stopping = true; // A service also closes its HTTP/queue admission here.
  void close().catch(error => { console.error(error); process.exitCode = 1; });
}
process.on("SIGTERM", stop);
process.on("SIGINT", stop);
initialized = (async () => {
  daemon = await makeDaemon({ auth: accessTokenFromEnv() });
  if (!stopping) current = await makeQuery({ prompt, options: {
    daemon, cwd: process.cwd(), settingSources: [],
  } });
})();
try {
  await initialized;
  if (!stopping) {
    for await (const message of current) {
      if (stopping) break;
      console.log(JSON.stringify(message));
    }
  }
} finally {
  try { await close(); }
  finally {
    process.removeListener("SIGTERM", stop);
    process.removeListener("SIGINT", stop);
  }
}
```
## Launch and validate

```bash
aw validate --config /absolute/aw.yaml
aw run --config /absolute/aw.yaml --agent sdk -- "Your task"
aw status --config /absolute/aw.yaml
aw stop --config /absolute/aw.yaml
```

`startDaemon()` confirms prewarm readiness; `createQuery()` confirms Query readiness after the SDK handshake. [Validate native behavior](../validation.md), including allow, block, Post failure and cleanup.
For security composition, use the [SDK example](../../../../../src/aw/crates/aw-service/examples/aw.sdk-security.yaml), replace paths and start sec-core first.

## Application constraints

| Item | Requirement |
| --- | --- |
| Shutdown | Handle TERM/INT in both ordinary and `--reloadable` workers; stop admission and await cleanup before exiting. Without handlers, Node exits immediately and AW forcibly reclaims remaining SDK children. |
| Factory coverage | Route every protected Query through the Bridge; direct native factories bypass AW checks |
| Native settings | Use `settingSources: []`; omit settings, extra CLI arguments and transport overrides. Imports and local plugins require [execution-asset grants](../configuration.md#qoder-sdk-execution-assets). |
| Permissions and input | Preserve native denial; argument rewriting and permission Hook overrides are unsupported |
| Environment and authentication | Queries sharing a daemon cannot override env/auth; use native `daemon.auth()` for credential changes or separate daemons for different credentials |
| Post checks | Observation and `withhold_result`; no result replacement |
| AgentSight | Ordinary resident mode requires `evidence.mode: off` |
| Worktree | Ordinary callbacks carry parent Query cwd, so policies needing actual execution cwd cannot use worktrees; restricted mode rejects them |

Cleanup waits for native Query, Hook, permission, credential and MCP callbacks to drain. Failed or timed-out private lease cleanup fences new work; fix the cause and restart the worker.

## Managed MCP and private execution

Use `await bridge.createSdkMcpServer(options)`, creating a new server for each Query. The pinned version provides Query identity; `parentIdentity: "required"` requests a model tool ID and is rejected.
AW rejects schema default/transform changes to approved arguments before entering the handler. Post failures use `withhold_result`; native structured results are retained.

Opt into private execution with `options.awExecutionFactory`. Handlers consume `executionEnvironment()`; results are redacted before Post and delivery. Built-in Bash has no environment injection seam.
See [private execution](../../../../../src/aw/docs/design/en/adapters/private-execution.md) for API and cleanup ordering, and [structured MCP](../../../../../src/aw/docs/design/en/adapters/structured-mcp.md) for identity.

## Native Bash command consumers

Use `options.awNativeExecutionFactory` when built-in Bash must run through an
application-owned credential consumer. Grant the wrapper executable in the
[execution-asset configuration](../configuration.md#qoder-sdk-execution-assets)
first. Ordinary daemon and direct Queries support this factory; `--reloadable`
rejects it.

Merge the options below into the existing Query factory, retaining `daemon`,
`cwd` and `settingSources: []`. The application supplies `approveTool`, its
native permission callback, and `acquireConsumer`, which acquires a command-bound
ticket and owns the execution process tree.

```javascript
export function nativeBashOptions(bridge, approveTool, acquireConsumer) {
  return {
    permissionMode: "default",
    canUseTool: approveTool,
    awNativeExecutionFactory: async (context, control) => {
      const consumer = await acquireConsumer(context, control);
      return {
        version: bridge.NATIVE_COMMAND_WRAPPER_VERSION,
        commandWrapper: {
          executable: consumer.executable,
          ticket: consumer.ticket,
        },
        settle: (control) => consumer.settle(control),
        release: () => consumer.release(),
      };
    },
  };
}
```

Every wrapper Query requires an explicit `canUseTool` callback and `default` or
`acceptEdits` mode. Creation and permission changes reject bypass/auto and other
modes. Every Bash Hook also checks the Worker-reported mode before acquisition.
PreTool retains the logical command through native rule evaluation; only an
explicit callback allowance publishes the wrapper through the SDK permission
result. Application Hooks cannot rewrite the approved input.

The factory receives frozen `{invocationId, nativeToolUseId, sessionId,
policyRevision, toolName, input}` and `{signal}`. Its lease accepts exactly the
four fields shown above; `commandWrapper` accepts only `executable` and `ticket`.
The ticket is 16–256 base64url characters. The admitted executable receives two
arguments: the ticket and the base64url-encoded original command. Bind the ticket
to that command, isolate credentials, preserve the exit status and redact output.
Native rules, the permission callback, AW policies and application Post input
use the logical command; later execution metadata can show the wrapper handle.
Use an opaque, short-lived ticket and keep it valid while approval is pending.

`settle({signal})` must confirm that the process tree and consumer finalizers
have ended, including after native timeout-backgrounding. Normal completion has
no fixed task-length cap. Cancellation asks the consumer to stop and join within
the shared cleanup deadline; bound acquisition and honor its signal. Clean up
partial acquisition before throwing.
AW holds finite-input EOF and native shutdown until consumer acknowledgement,
then drains native work and original callbacks before `release()` revokes the
ticket. Native rejection before publication releases the unused ticket without
calling `settle`. If settlement or cleanup fails, retain ownership, diagnose the
failure and restart the fenced worker. Ordering invariants are in the
[execution design](../../../../../src/aw/docs/design/en/adapters/private-execution.md#native-bash-consumers).

## Updates and long-running work

Ordinary mode [updates policies](../deployment/policy-updates.md) after all Queries fully close; the next Query takes the new snapshot.
Restricted `--reloadable` can update between requests in one Query. It requires an explicit daemon, `msg_lifecycle_v1` and native idle confirmation, and rejects strict Post, private leases, goals, background scheduling and worktrees.
Consume Query output continuously; if consumption stops, stop dispatch and call `close()` or `return()`. This mode has no fixed output buffer limit and retains the 1024 Hook association and 64 active claims per kind limits.
See the [update contract](../../../../../src/aw/docs/design/en/runtime/policy-reload.md) for the complete restricted entrypoint list.
