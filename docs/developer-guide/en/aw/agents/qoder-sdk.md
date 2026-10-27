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
const daemon = await makeDaemon({ auth: accessTokenFromEnv() });
let current;
try {
  current = await makeQuery({ prompt, options: {
    daemon, cwd: process.cwd(), settingSources: [],
  } });
  for await (const message of current) console.log(JSON.stringify(message));
} finally {
  await current?.return();
  await daemon.stop();
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
| Factory coverage | Route every protected Query through the Bridge; direct native factories bypass AW checks |
| Native settings | Use `settingSources: []`; omit settings, plugins, extra CLI arguments and transport overrides |
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

## Updates and long-running work

Ordinary mode [updates policies](../deployment/policy-updates.md) after all Queries fully close; the next Query takes the new snapshot.
Restricted `--reloadable` can update between requests in one Query. It requires an explicit daemon, `msg_lifecycle_v1` and native idle confirmation, and rejects strict Post, private leases, goals, background scheduling and worktrees.
Consume Query output continuously; if consumption stops, stop dispatch and call `close()` or `return()`. This mode has no fixed output buffer limit and retains the 1024 Hook association and 64 active claims per kind limits.
See the [update contract](../../../../../src/aw/docs/design/en/runtime/policy-reload.md) for the complete restricted entrypoint list.
