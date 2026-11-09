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

For strict output inspection, set the sec-core `scan_tool_output` step to
`on_error: withhold_result`. Both Pi packages must be 0.84.2. Configuring any strict
after route suppresses intermediate tool updates across the target; subscribers
and the model receive only the adopted final result. AW scans a detached snapshot
of the effective result and delivers that snapshot, including native after-callback
overrides and metadata. Later mutations of retained result objects do not change
the delivered result. PII risk replaces text;
scanner faults discard the full result and return a fixed placeholder. The same
session can continue after a scanner fault. Targets without strict routes retain
native streaming and report-mode Tokenless.

## Native startup and idle policy updates

A native Node host can prepare AW once at startup, then run the existing
`createSession`/`runWorker` factory in its own process. Keep `aw.yaml` in its
working directory; target `pi` must identify the factory application and its
installed SDK packages. Add the following bootstrap to the native host and
restart it once. Run the host with `node native-host.mjs`.

```javascript
import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";

const aw = "aw";
const prepared = JSON.parse(execFileSync(aw,
  ["instance", "prepare", "--agent", "pi"], { encoding: "utf8" }));
Object.assign(process.env, prepared.environment);
try {
  const { runNativeWorker } = await import(pathToFileURL(prepared.command.args[0]));
  await runNativeWorker({ application: prepared.command.args[1], args: prepared.command.args.slice(2) });
} finally {
  execFileSync(aw, ["instance", "release", "--binding", prepared.binding]);
}
```

After the host reports Ready, use `aw instance update --agent pi` from the
configuration directory. Each session retains one immutable policy through
its prompts, tool callbacks and disposal. Updates reject an open session,
application factory preparation or unconfirmed cleanup; close every session
before updating. Newly created sessions adopt the new policy, including
input and Skill checks. Strict output updates still require both Pi packages
at 0.84.2. A preparation failure leaves dispatch disabled; release only after
all sessions have closed. Native startup requires the resident factory.

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
expanded slash commands other than the checked `/skill:name` path below,
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

## Validate Skill submissions

Use a separate Pi configuration for `input.submit/check_skill`, such as
`aw.pi-submission.yaml`. The Provider declares this event as Pi-only; selecting
Qoder SDK, QwenPaw or another Adapter from the same configuration fails admission
before native hooks are ready, with either `on_error: block` or `report`.
Input events have no Agent selectors. Run other Agents with a separate configuration
that omits this input Skill step while retaining their prompt/PII steps and applicable
tool-time Skill checks. Give the configurations distinct `metadata.name` values when
using automatic daemon paths, or distinct explicit `state_dir` and endpoints.

Enable `config.skill: true` and put `check_skill` before `scan_prompt` and
`scan_user_input` on `input.submit`, as in the submission example above. Authorize
and sign each canonical Skill directory using the [Skill Ledger](../../../../user-guide/en/agent-sec-core/skill-ledger.md).
Then submit `/skill:name optional arguments` through the managed `session.prompt`.
A valid signed Skill reaches the model; denied trust, changed source and a failed
required check reject the prompt before model transport.

For Coding Agent 0.84.2, AW selects the first matching object from the session's
actual `resourceLoader.getSkills()`, fixes its directory `SKILL.md` text, and uses
the SDK frontmatter parser and native command format. The final expanded text,
including arguments, passes through all input checks. Native submission receives
`expandPromptTemplates: false`, so it cannot select or expand a second object.
Files are bounded to 1 MiB and rechecked after policy evaluation. The Provider also
uses the existing V2 Ledger `manifestHash` to authenticate the local manifest,
then compares its `fileHashes["SKILL.md"]` with the captured raw-file digest.
This prevents directory swaps or forged manifest labels from approving another
snapshot, using the existing backend interface. See the [Provider contract](../providers/sec-core.md#validate-skills-before-loading)
for artifact access and limits. Use `on_error: block` for required Skill trust. A source change
or unresolved source rejects adoption, including with `on_error: report`.

Ordinary text and explicitly literal slash text carry explicit no-Skill metadata;
`check_skill` returns `skill_not_requested` without contacting the Ledger. Unknown
Skills, flat Markdown files and a same-named extension command have no checked
source. Automatic context injection, direct file reads and Skill commands need
policies at their own execution points. A bare pi-agent-core application must
supply its business Skill loader before this Coding Agent source contract applies.
