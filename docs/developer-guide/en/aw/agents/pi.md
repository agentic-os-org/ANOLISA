# Pi SDK

[中文版](../../../zh/aw/agents/pi.md)

Run source examples from `src/aw`; build AW with the [integration tutorial](../integration.md) first.

## Start a Pi SDK worker

For a high-code Pi application, use `adapter: pi` with the public Coding Agent/Core
SDKs and Node.js >=22.19.0. Both SDKs at 0.84.2 are the verified baseline.
AW checks package identity, module layout and native Agent/AgentSession interfaces
for other releases, and records the installed Coding Agent version. AW owns a foreground SDK worker and connects
both tool Hook points. Install those SDK packages in your application using your
normal dependency workflow, and retain your application's model/auth settings.
The native Pi CLI is a separate entrypoint and is not launched by this adapter.

Expose your existing public factory and foreground task in an absolute `.mjs`
application module:

```javascript
export async function createSession({ sdk }) {
  return sdk.createAgentSession({
    cwd: process.cwd(),
    agentDir: "/absolute/existing-pi-agent-directory",
  });
}

export async function run(session, { args }) {
  await session.prompt(args.join(" "));
}
```

Copy the [Pi example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.pi.yaml),
set its `argv` to `[node, /absolute/application.mjs]`, and configure your before
and after policies. Its included commands are neutral. Then run:

```bash
target/debug/aw validate --config /absolute/aw.pi.yaml
target/debug/aw run --config /absolute/aw.pi.yaml --agent pi -- 'Your task'
target/debug/aw status --config /absolute/aw.pi.yaml
target/debug/aw stop --config /absolute/aw.pi.yaml
```

`--agent` selects your configuration target; its `adapter` remains `pi`.
`--native-settings`, `--native-profile` and `--native-state-dir` are not Pi SDK
options. AW validates the installed SDK versions before binding, waits for the
factory and installed callbacks, and then reports `AW pi native hooks ready`.
The application task must stay in that foreground worker. For replacement
new/resume/fork/import sessions, use the `createSession` supplied to `run` so each
factory result gets the same Hook composition. Existing before denial/mutation
and after replacement remain authoritative; AW after observes the final result.
Blocked or invalid/pre-aborted tools have no after callback in Pi.

For Tokenless, use the Pi SDK target in the [shared optimization example](https://github.com/agentic-os-org/ANOLISA/blob/main/providers/tokenless/examples/aw.yaml).
Keep security checks before `pre_tool` and observers/custom Hooks before the final
`post_tool` replacement. Retry reasons reach the model; only a fresh tool call can
execute the suggested command. PostTool replaces one successful text block while
preserving SDK metadata; report failures retain the original result. Raw native
Hooks cannot supply replacements. AW uses a fresh invocation UUID for pairing,
retains the original tool ID in `native`, and closes events on early denial.

See the [adapter contract](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/docs/design/en/adapters/pi-adapter.md)
for exact callback semantics, identity, cancellation, capacity and reproducible
SDK acceptance. Exit Agents and stop the original service before changing policy;
retain/remove the audit state using the same local-service cleanup procedure.

## Check user submissions

Use the [Pi submission example](../../../../../src/aw/crates/aw-service/examples/aw.pi-submission.yaml)
with a resident factory exporting `runWorker`. Replace the executable and sec-core
socket paths, retain your application's model/auth configuration, and launch with
`aw run --config /absolute/aw.pi-submission.yaml --agent pi -- 'Your task'`.
The application exposes its existing factory plus this worker entrypoint:

```javascript
export async function createSession({ sdk, cwd }) {
  return sdk.createAgentSession({ cwd, agentDir: "/absolute/existing-pi-agent-directory" });
}

export async function runWorker({ args, withSession }) {
  await withSession({ cwd: process.cwd() }, async (session) => {
    await session.prompt(args.join(" "));
  });
}
```

`session.prompt(text, options)` awaits the configured `scan_prompt` and
`scan_user_input` steps before entering native prompt execution. The original text,
including Chinese, whitespace and empty strings, reaches Pi unchanged. A policy
denial or `on_error: block` failure rejects the promise; catch it to submit another
prompt in the same session. `on_error: report` records the failed check and continues.
A rejected check calls `preflightResult(false)` when provided.

When input checking is configured, the supported source is interactive text.
Images/attachments, `input`, `context` and `before_provider_request` extension handlers,
expanded slash commands,
`steer`, `followUp`, `sendUserMessage`, `sendCustomMessage`, direct Agent submission
and concurrent prompts are rejected. Use `expandPromptTemplates: false` to submit
literal slash text. Native prompt options are retained; model continuations and
tool retries remain part of the existing execution and do not create submissions.
The final Agent prompt must contain the exact checked user text. Context and
provider-request transformations are rejected because they can rewrite that text
after this check. Applications
using these unsupported input paths must remove the input event or use a supported
text-only factory. Direct `Agent.transformContext` and `Agent.convertToLlm` callbacks
must preserve the ordered user-message content, including existing history;
`Agent.onPayload` must leave the serialized request unchanged. AW isolates callback
inputs/outputs and rejects rewrites before delivery. Callback slots are fixed for
the managed session. Conversions that add user messages, including custom/summary
messages, require a factory without input checking. Without `input.submit`, existing
Pi input modes and callbacks remain available.

Await `session.abort()`, `session.dispose()` or the factory's `close()` to cancel
and release a managed task. Cancellation is terminal for that session: create a
fresh factory session afterward. Teardown waits up to 12 seconds for native abort,
pending prompt/check cleanup and disposal, then releases the AW scope. A failed
cleanup requires worker restart. A late scan reply cannot dispatch a model request.
