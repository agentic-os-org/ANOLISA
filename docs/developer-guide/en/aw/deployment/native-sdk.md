# Application-owned Runtime SDK

[中文版](../../../zh/aw/deployment/native-sdk.md)

Use `@anolisa/aw-sdk` or the asynchronous `anolisa-aw-sdk` Python package when
an application owns startup and needs shared AW policy and lifecycle management.
Both core packages provide connection transport, `Instance`, `Task`, `Admission`,
`TaskScope`, Deployment Descriptor loading, cooperative control, and the
framework-free language-level Source Runtime (`@anolisa/aw-sdk/sources` and
`aw_sdk.sources`). Framework-specific integrations ship as separate optional
adapter packages (`@anolisa/aw-sdk-qoder`, `@anolisa/aw-sdk-pi`,
`anolisa-aw-sdk-qwenpaw`, and `anolisa-aw-sdk-claude`) rather than being bundled
into the core packages. Linux is required; the Node packages need Node.js 22+.
Install AW and Providers separately. These source-built candidate packages await
publication through the AW release process.

Build AW from the same candidate revision as the language package. Existing
releases without `aw-sdk/v1` reject connection; cooperative control additionally
requires `aw-sdk-control/v1`. These source-built packages do not upgrade AW.

## Package layout (6 packages)

AW ships 2 framework-free core packages and 4 optional framework adapter packages
(3 Python wheels and 3 npm tarballs). Obtain the core and dual-language SDK
packages from your applicable distribution channel (such as OS packages when
available, published release/CI archives, or a
[source build](../../../../../contribute/en/aw/building.md)), and verify artifact
integrity following the [installation guide](../../../../user-guide/en/aw/installation.md#get-a-package).
Each adapter package pins the matching core package version and is installed
only when your application uses that framework:

| Language | Package | Role | Primary import | Descriptor profile |
| --- | --- | --- | --- | --- |
| Python (`>=3.11`) | `anolisa-aw-sdk` (core) | Transport, `Instance`/`Task`/`Admission`/`TaskScope`, Descriptor loader, cooperative control, `LanguageSource` | `aw_sdk`, `aw_sdk.sources` | — (`adapter: sdk`) |
| Python (`>=3.11`) | `anolisa-aw-sdk-claude` (optional) | Resident Claude Agent SDK worker | `aw_sdk_claude` (`ResidentClaude`) | `claude-resident-hooks` |
| Python (`>=3.11`) | `anolisa-aw-sdk-qwenpaw` (optional) | QwenPaw native plugin & deferred after-only plugin | `aw_sdk_qwenpaw` (`QwenPawPlugin`, `AfterOnlyPlugin`) | `qwenpaw-installed-native-hooks` |
| TypeScript / Node (`>=22`) | `@anolisa/aw-sdk` (core) | Transport, `Instance`/`Task`/`Admission`/`TaskScope`, Descriptor loader, cooperative control, `LanguageSource` | `@anolisa/aw-sdk`, `@anolisa/aw-sdk/sources` | — (`adapter: sdk`) |
| TypeScript / Node (`>=22`) | `@anolisa/aw-sdk-qoder` (optional) | Public-Hook Qoder Agent SDK bridge | `@anolisa/aw-sdk-qoder` (`createQoderBridge`) | `qoder-public-hooks` |
| TypeScript / Node (`>=22.19`) | `@anolisa/aw-sdk-pi` (optional) | Resident Pi coding agent worker | `@anolisa/aw-sdk-pi` (`runPiWorker`) | `pi-worker-hooks` |

Choose your integration path:

- **Custom Python or Node.js Agent (zero Rust Adapter)**: Install only the core
  package and instrument input/tool methods with `LanguageSource`
  ([Framework-free integration](#framework-free-integration)).
- **Resident framework application**: Install core + the matching optional
  adapter package, compile startup hints with `aw config descriptor`, start the
  application natively, and attach with `aw connect --config /absolute/aw.yaml --sdk-pid APPLICATION_PID`
  ([Deployment Descriptor](#deployment-descriptor)).

## Configuration-driven Qoder integration

Install `@anolisa/aw-sdk` together with the optional `@anolisa/aw-sdk-qoder`
adapter package. Use `createQoderBridge` (imported from `@anolisa/aw-sdk-qoder`
or forwarded through `@anolisa/aw-sdk/qoder`) to retain the native Node entrypoint, daemon, authentication,
model settings, permissions and Hook groups. The bridge reads the same owned
`aw.yaml` used by deployment, installs public Qoder Hooks and publishes cooperative
attachment support. A deployment-generated Descriptor lets initialization read
validated wiring without starting a configuration-parser subprocess. Without
`descriptorFile`, AW's offline `config show` parser reads YAML or JSON once through
a bounded CLI invocation. Neither path starts AW or sec-core.

```javascript
import { createQoderBridge } from "@anolisa/aw-sdk-qoder";
import { query } from "@qoder-ai/qoder-agent-sdk";

const aw = await createQoderBridge({
  configFile: "/absolute/aw.yaml",
  query,
});
const session = aw.query({ prompt, options: nativeOptions });
for await (const message of session) deliver(message);
// At application shutdown:
await aw.close();
```

Set `spec.daemon.endpoint` or `spec.daemon.state_dir` to an explicit private
absolute path; this bridge rejects configurations with both paths set to `auto`.
A single `qoder-sdk` target is selected automatically; pass `target` when the
configuration contains several. The bridge defaults to `availability: "native_fallback"`:
AW loss leaves native Queries, Hooks, tools and model execution running. Set
`required: false` on every enabled event applicable to this target; `on_error: block` and explicit denials
still apply while AW is reachable. A conflicting required policy rejects bridge
initialization. Choose `availability: "required"` explicitly when protected work
must stop on AW loss. Availability is an SDK lifecycle option; policy remains in the file.
The `qoder-public-hooks` profile supports configured input and tool-before
observation and blocking, plus tool-after observation and `replace_result`.
A replacement step uses `on_error: report` and runs last after the observation
steps. All steps receive the original tool result. The adapter returns
`hookSpecificOutput.updatedToolOutput` on `PostToolUse` for a supported text
slot and preserves the rest of the tool result. `PostToolUseFailure` provides error details for observation
and has no replacement slot. A Provider error is surfaced to the application
and requests a native Query stop. This stop does not establish that Qoder
withheld an unchecked result. For `withhold_result` or a final-result gate, use
a verified managed entrypoint.

After native application readiness, deployment prepares sec-core in the background
and runs under the application user:

```bash
aw connect --config /absolute/aw.yaml --sdk-pid APPLICATION_PID --timeout-ms 10000
```

With `daemon.startup: on_demand`, this command starts or reuses AW. The PID must
belong to the Node application running the installed bridge. The bridge installs
`SessionStart` and `UserPromptSubmit` only for their respective enabled events.
With both events disabled, model inference can overlap attachment and the first
`PreToolUse` fixes the Task policy. Required admission waits up to
`attachTimeoutMs` (5000 ms by default). Native fallback can proceed when AW is
unavailable. An internal tool-before observation still correlates an
after-only policy. Once admitted, each Query uses one Scope.

Current `qoder-public-hooks` deployment descriptors include the optional
`observe_session_start` hint when `session.start` is enabled for the target.
The bridge reads older descriptors with the hint absent as disabled. Regenerate the descriptor when enabling
`session.start`, and restart the bridge when changing either startup event.

`aw.query` accepts the original Query input and returns a proxy preserving native
methods. Consumption completion, early iterator return and explicit `close()`
close the native Query and its Scope. Application shutdown closes remaining
Queries. Each Query's `cwd` and copied environment define its task context;
`QODER_PERSONAL_ACCESS_TOKEN` stays with native authentication and is excluded from
AW Hook environments. Other sensitive variables should be excluded through the
bridge's `environment` option. Keep one AW integration per Query. This path uses
public Hooks and adds no module patch or preload to the native SDK.

## Pi worker integration

Install `@anolisa/aw-sdk` together with the optional `@anolisa/aw-sdk-pi`
adapter package on Node.js 22.19 or later (legacy `@anolisa/aw-sdk/pi` forwards
to `@anolisa/aw-sdk-pi` when installed).
Existing application modules export `createSession({ sdk, cwd, args, ...options })`
and `runWorker({ args, createSession, closeSession, withSession })`. Keep native
model setup, settings, tools and callbacks in the application factory. Use the
supplied `createSession` and `session.prompt()` for protected work.

```javascript
import { runPiWorker } from "@anolisa/aw-sdk-pi";

await runPiWorker({
  application: "/absolute/pi-worker.mjs",
  configFile: "/absolute/aw.yaml",
  descriptorFile: "/absolute/aw-pi.descriptor.json",
  args: process.argv.slice(2),
  automatic: false, // Set true to reattach automatically after worker restarts.
});
```

Compile the descriptor with the `pi-worker-hooks` profile at deployment so the
worker starts without `aw config show`; omit `descriptorFile` to use the offline
parser. Configure a Pi target with an explicit daemon endpoint or state
directory. The process publishes the SDK listener before entering the worker.
Pass `automatic: true` when a restarted resident worker should automatically
register and reconnect to the configured Service without external PID tracking
(`automatic` defaults to `false`).
Deployment attaches under the application's Linux user. AW is always required.

```bash
aw config descriptor --config /absolute/aw.yaml --agent pi \
  --profile pi-worker-hooks > /absolute/aw-pi.descriptor.json
aw connect --config /absolute/aw.yaml --sdk-pid APPLICATION_PID --timeout-ms 10000
```

Release support comes from the same Pi Adapter capability declaration that
`aw run` uses; the SDK entry adds no version check of its own. Any installed
Coding Agent and Agent Core release is accepted. The worker declares
`withhold_result` only for an identical stable pair inside the verified
0.84.2–1.5.2 range that also exposes the native event dispatch seam. Other
releases keep the tool Hooks, and a configuration that needs strict result
protection is rejected at registration rather than silently degraded. The SDK
input and strict-output suites run on the pinned 0.84.2, 1.3.0 and 1.5.2 releases.

AW is required for session creation. `attachTimeoutMs` bounds the wait for Ready
and defaults to 5000 ms. Each session uses one acknowledged Scope. The worker
validates and installs the native hooks before acknowledging the Scope. Close
the session to drain work and release its policy snapshot. Session Scopes reuse
the managed resident Scope protocol, carried by an SDK Task that owns
registration, acknowledgement, renewal and release. The existing
`run(session, { args, createSession, closeSession })` factory is also supported
with tool policies. Input submission requires `runWorker`; enabling it for a
worker that started with tool-only configuration requires a restart.

## Shared admission boundary

TypeScript and Python export `Admission` alongside `Instance` and `Task`.
A Source installs its native callbacks first, then creates one Admission for each
native request with the installed event set and copied workspace/environment.
Call `prepare()` before the earliest protected input or first tool. The common
SDK owns bounded Ready waiting, one Task, cancellation and close; the Source owns
native capture, field mapping and adoption. Service, Host and executor retain
policy admission and Provider execution.

An installed `input.submit` route waits for preparation even with optional
availability. Optional input may fall back only after its attachment deadline;
explicit errors and cancellation still propagate. Required tool admission waits;
optional tool-only admission fixes native fallback immediately when detached.
Calling `close()` cancels pending admission, so stop/drain native work before
releasing its Task. Sources must call `prepare()` at the appropriate boundary;
the SDK does not infer model dispatch or tool execution from a PID.

The Qoder bridge and configured [QwenPaw plugin](python-sdk.md#install-and-integrate)
reuse this common lifecycle. Deployment Descriptors use the existing installed
capability profiles. Automatic Source discovery and resolution remain planned.

## Deployment Descriptor

For resident framework applications, the cooperative attachment workflow has
three steps:

1. **Compile a config-bound Descriptor at deployment** (skips runtime YAML
   parsing and `aw` subprocess startup):

```bash
umask 077
aw config descriptor --config /absolute/aw.yaml --agent sdk \
  --profile qoder-public-hooks > /absolute/aw-sdk.json
```

2. **Start the application natively** with both absolute paths (`configFile` and
   `descriptorFile`) so it installs native Hooks and publishes its SDK listener:

```javascript
const aw = await createQoderBridge({
  configFile: "/absolute/aw.yaml",
  descriptorFile: "/absolute/aw-sdk.json",
  query,
});
```

3. **Attach cooperatively by PID** under the same Linux user once the
   application and any external Provider backends are ready:

```bash
aw connect --config /absolute/aw.yaml --sdk-pid APPLICATION_PID --timeout-ms 10000
```

With `daemon.startup: on_demand`, `aw connect --sdk-pid` starts or reuses AW,
waits for the process to publish its SDK listener and confirms Ready without
launching the Agent or injecting into arbitrary PIDs.

Generate the Descriptor using the same AW build and exact configuration file
that the application and Service will use. Check the command's exit status
before publishing the output; generate into a staging location and install the
JSON together with `aw.yaml` before starting the application.
If a `claude-resident-hooks` Descriptor contains a nonempty
`allow_plaintext_http_hosts` list, install the Descriptor with all write bits
cleared (for example, mode `0400`).

| Target framework | `--profile` | SDK entrypoint |
| --- | --- | --- |
| Qoder SDK | `qoder-public-hooks` | `createQoderBridge(..., descriptorFile)` (`@anolisa/aw-sdk-qoder`) |
| Pi SDK | `pi-worker-hooks` | `runPiWorker({ ..., descriptorFile })` (`@anolisa/aw-sdk-pi`) |
| QwenPaw | `qwenpaw-installed-native-hooks` | `QwenPawPlugin.from_config(..., descriptor_file=...)` (`anolisa-aw-sdk-qwenpaw`) |
| Claude SDK | `claude-resident-hooks` | `ResidentClaude.from_config(..., descriptor_file=...)` (`anolisa-aw-sdk-claude`) |

A profile version names the Hook contract and does not track the native package version.
Compilation applies the existing capability admission rules and records the
target, profile, explicit socket, input boundary and availability. It starts no
Service, Provider or native application. The QwenPaw profile requires service
availability; Qoder records whether applicable events require protection. In both
loading paths, Qoder retains the caller's availability choice and defaults to
`native_fallback`. Pass `availability: "required"` explicitly when the descriptor
requires it; a conflicting choice fails before installing the listener.

SDK loading validates the schema version, exact profile and optional explicit
target, then hashes the raw configuration bytes. Each input must be an application-user-owned regular
file of at most 4 MiB, with one hard link, no special mode bits and no group/world
write permission. Use an absolute path without symlinks or `..`; the immediate
parent must also be application-user-owned and not group/world writable. A missing, malformed, mismatched or stale Descriptor fails
initialization; an explicit Descriptor never falls back to `config show` or
native execution. Even a whitespace-only configuration edit requires regeneration.
The loading path invokes no AW executable and parses no YAML.

The Descriptor compiler and both SDK readers support ext2/3/4, XFS, Btrfs, tmpfs and OverlayFS,
checking both the open file and its pinned parent. Other filesystems are rejected;
select the existing CLI-loading API explicitly when deploying there. Node accepts
read-only POSIX ACL entries when their effective write mask passes the mode check.
Python and the CLI additionally reject extended file ACLs and native ACLs on the
parent. Deploy plain files without extended ACLs for the same behavior in both
languages.

The Descriptor and configuration, including their parent directories, must be
controlled by the trusted deployment owner. The digest detects a mismatched
configuration; it does not authenticate the publisher or prevent a writer from
changing wiring hints. Service still owns policy admission and execution.
Drain the application, deploy a matching pair and restart when wiring changes.
To return to offline CLI loading, omit `descriptorFile` / `descriptor_file` and
restart with the corresponding AW executable available.

## Framework-free integration

Configure a target named `app` with `adapter: sdk` and the application's ordinary
`argv` (the application starts itself; `aw run` does not launch `adapter: sdk`):

```yaml
agents:
  app:
    adapter: sdk
    argv: [/usr/bin/env, python3, /absolute/path/to/agent.py]
```

Start the AW service with the complete `aw.yaml` and export its private socket
as `AW_SOCKET`:

```bash
aw service serve --config /absolute/aw.yaml
```

For any custom Node.js/TypeScript or Python Agent, install only the core SDK
package and use `LanguageSource` (`@anolisa/aw-sdk/sources` or `aw_sdk.sources`)
to instrument input and tool methods with zero Rust Adapter code:

**Node.js / TypeScript (`@anolisa/aw-sdk/sources`)**:

```javascript
import { LanguageSource, SourceDenied } from "@anolisa/aw-sdk/sources";

class CustomAgent {
  async ask(prompt) { return this.echo({ text: prompt }); }
  async echo({ text }) { return `echo:${text}`; }
}

const agent = new CustomAgent();
const source = LanguageSource.create({
  endpoint: process.env.AW_SOCKET,
  target: "app",
  availability: "required",
});

try {
  await source.connect();
  await source.withSession({ sessionId: "example-js-agent" }, async (session) => {
    session.instrument(agent, {
      inputMethods: ["ask"],
      toolMethods: { echo: "echo" },
    });
    try {
      console.log(await agent.ask("hello"));
    } catch (error) {
      if (error instanceof SourceDenied) console.log(`AW denied ${error.event}`);
      else throw error;
    }
  });
} finally {
  await source.close();
}
```

**Python (`aw_sdk.sources`)**:

```python
import asyncio
import os
from aw_sdk.sources import LanguageSource, SourceDenied


class CustomAgent:
    async def ask(self, prompt: str) -> str:
        return await self.echo(text=prompt)

    async def echo(self, *, text: str) -> str:
        return f"echo:{text}"


async def main() -> None:
    agent = CustomAgent()
    async with LanguageSource.create(
        endpoint=os.environ["AW_SOCKET"],
        target="app",
        availability="required",
    ) as source:
        async with source.session(session_id="example-python-agent") as session:
            session.instrument(
                agent,
                input_methods=["ask"],
                tool_methods={"echo": "echo"},
            )
            try:
                print(await agent.ask("hello"))
            except SourceDenied as denied:
                print(f"AW denied {denied.event}")


asyncio.run(main())
```

Behavioral contract of the language-level Source Runtime (`@anolisa/aw-sdk/sources`
and `aw_sdk.sources`):

- Create a source with `LanguageSource.create(...)` in either language, building
  canonical `aw-hook-source/v1` capabilities and route confirmation (`availability`
  supports both `required` and `native_fallback`).
- Open one session per request (`source.withSession(...)` / `source.session(...)`
  in TypeScript; `async with source.session(...)` in Python) owning one
  `Admission` and immutable `Task`. Narrowed session event lists cannot skip
  admitted Task routes (`source_route_coverage_incomplete`).
- Wrap callables or patch object methods with `session.wrapInput`,
  `session.wrapTool`, `session.wrapInputStream` / `session.wrapToolStream`, and
  `session.instrument` in TypeScript, or `session.wrap_input`,
  `session.wrap_tool`, and `session.instrument` in Python. Blocked `input.submit`
  and `tool.before` calls raise `SourceDenied` before native execution.
  Input and tool wrappers deep-copy plain JSON arguments before admission and
  use those copies for extraction and native execution, including the first
  stream read. For class instances or other application-specific arguments,
  supply `snapshotArg` (TypeScript) or `snapshot_arg` (Python). It must return a
  separate argument of the expected native type and copy all mutable input the
  callable reads, including during iteration. For example, a request containing
  only a text field can use `snapshotArg: req => new RequestData(req.text)`.
  An argument that needs a copy raises `source_input_snapshot_required` when
  the callback is absent or returns the original object.
  Set the callback in the method's `toolMethods` or `tool_methods` options
  when using instrumentation. Python also accepts `snapshot_arg` on
  `instrument` as a default for its methods. Custom tool input extractors
  should read the copied arguments and include every field that policy needs to
  check. The SDK rechecks that projection before dispatch.
- For direct iteration in TypeScript, use `wrapInputStream`, `wrapToolStream`
  or `{ stream: true }` with either a generator declaration or a factory. With
  implicit detection, a native async generator returns an iterator and an
  ordinary factory returns a Promise. Their TypeScript call signatures can be
  identical, so `await wrapped(...args)` handles either return shape. Select an
  explicit stream wrapper when a factory needs SDK-managed stream cleanup or
  tool result collection.
- A Task with an admitted `tool.after` route collects stream items up to
  `maxStreamItems` / `max_stream_items`. When `extractResult` / `extract_result`
  is set, it receives the collected native chunks (with plain JSON chunks copied
  before yielding). A before-only Task or a Task in native
  fallback passes through native results and stream items. Errors in result
  extraction, normalization or collection carry `toolExecuted` / `tool_executed`.
- Synchronous wrapped callables execute on the event loop thread; use `async`
  callables or offload blocking work (such as `asyncio.to_thread` in Python) so
  cancellation, concurrent sessions and `close` timeouts remain responsive.
- Session close stops active streams and waits for admitted native calls,
  in-flight `submit` / `beforeTool` / `before_tool` checks and
  their `tool.after` observations before restoring patched methods and releasing
  the Task. Python cancels the pending native read. In Node, pass `session.signal`
  to blocking native reads. For caller cancellation, combine it with the caller's
  signal using `AbortSignal.any` and pass that signal to both the read and
  `extractSignal`. The SDK preserves direct `AbortSignal` arguments by reference.
  If pending work exceeds the close timeout (defaulting to the session timeout,
  or overridden per call with `{ timeoutMs }` / `timeout_ms`), close reports
  `source_close_pending` while keeping patched methods and the Task in place as
  work drains. Unblock the native work and await close again, including
  `source.close()` if the source owns the Instance. After cancellation, session
  close releases the Task and reports native cleanup failures, including those
  from in-flight reads or stream factories. Once `source.close()` starts, all
  existing sessions close concurrently and new sessions fail with `source_closed`.
  Retrying a timed-out Source close also reports a native cleanup failure that
  occurred between attempts.
- One method on a given object instance may be instrumented by only one active
  `SourceSession` at a time (`source_already_instrumented`); create per-request
  object instances or wrap functions directly when serving concurrent sessions.

### Rewrites and fault withholding (`replace_input`, `replace_result`, `withhold_result`)

Opt in per `LanguageSource` to keep existing observe/block callers unchanged:

- Pass `replaceInput: true`, `replaceResult: true`, and/or `withholdResult: true`
  to `LanguageSource.create(...)` in TypeScript (or `replace_input=True`,
  `replace_result=True`, `withhold_result=True` in Python). Route confirmation
  rejects any policy step whose effect or `withhold_result` failure action was
  not declared.
- **`replace_input`**: When a `tool.before` Provider returns approved revised
  arguments, `wrapTool` / `wrap_tool` rebuilds plain JSON dict/object arguments
  safely (or calls optional `applyInput` / `apply_input` for custom argument
  shapes) and re-verifies the projected input before native execution. Shapes
  that cannot be rebuilt safely fail closed with `source_input_replace_unsupported`
  rather than silently discarding the rewrite.
- **`replace_result`, `withhold_result` and strict output isolation**: When
  `tool.after` returns a replacement, `wrapTool` / `wrap_tool` replaces string
  or plain structured text envelopes (or calls optional `applyResult` /
  `apply_result`); unsupported shapes fail with `source_result_replace_unsupported`.
  On strict-after routes (`replace_result` or `on_error: withhold_result`):
  - Without `extractResult` / `extract_result`, the SDK only accepts strict
    plain JSON values (in TypeScript, dense plain arrays with no extra named,
    non-enumerable, symbol, or accessor properties, and plain objects with
    enumerable string data properties; a top-level `undefined` return from a
    non-streaming TypeScript tool is preserved as `undefined` on delivery while
    projected as `null` in the audited `tool.after` payload, whereas `undefined`
    inside objects/arrays or in stream chunks is rejected) and delivers a
    detached copy of the **exact audited JSON value**. Opaque custom types,
    sparse arrays, arrays with extra properties, and unbuffered lazy
    iterators/generators fail closed (`source_result_isolation_required` or
    `source_stream_withhold_requires_buffering`).
  - With `extractResult` / `extract_result`, AW evaluates the extractor once
    on an isolated copy and audits the normalized JSON projection it returns.
    On non-streaming strict routes, opaque non-JSON objects still fail closed
    unless the caller also provides `snapshotResult` / `snapshot_result` to
    detach the native value; in that case AW audits the extracted projection
    rather than opaque native fields.
  - When a required `tool.after` check fails under `on_error: withhold_result`,
    the SDK raises `SourceResultWithheld` (`toolExecuted` / `tool_executed = true`)
    and never returns unchecked output. Under `on_error: report`, audited
    recoverable Provider faults on `tool.after` are recorded and allow the
    checked original result through, while Scope/RPC/cancellation/audit faults
    remain fail-closed.
- **Honest streaming boundary and unified byte budget**: Unbuffered streaming
  tools cannot recall chunks already yielded to callers. When `tool.after`
  admits `replace_result` or `on_error: withhold_result`, an unbuffered stream
  (async generator or sync generator/iterator) fails before starting the native
  generator with `source_stream_withhold_requires_buffering`. Set
  `bufferStream: true` / `buffer_stream=True` to buffer isolated strict plain
  JSON chunk snapshots up to both `maxStreamItems` / `max_stream_items` and
  `maxStreamBytes` / `max_stream_bytes` (default `1048576` bytes). This byte
  budget applies only to buffered streams (ordinary non-streaming tool results
  do not use `maxStreamBytes` / `max_stream_bytes`) and is metered in compact
  UTF-8 JSON (`JSON.stringify` in TypeScript; `separators=(",", ":")`,
  `ensure_ascii=False` in Python; wire RPC framing may be larger when ASCII
  escapes are used). Strict stream buffering rejects unmeterably opaque custom
  types (`source_result_isolation_required`) even when `extractResult` /
  `extract_result` or `snapshotResult` / `snapshot_result` is configured, and
  enforces `maxStreamBytes` / `max_stream_bytes` on both the buffered chunk
  snapshot array (including `[]` at 2 bytes for an empty stream) and the
  single-evaluated audited `tool.after` payload (including after
  `extractResult` / `extract_result`). Overflow or withholding closes the
  underlying iterator and yields zero chunks.

See the [TypeScript Source example](../../../../../src/aw/sdk/typescript/examples/language-source.mjs)
and [Python Source example](../../../../../src/aw/sdk/python/examples/language_source.py).

For low-level direct callbacks, use the
[TypeScript example](../../../../../src/aw/sdk/typescript/examples/application.mjs)
or [Python example](../../../../../src/aw/sdk/python/examples/application.py), set
`AW_SOCKET` to its private socket, and install the matching core language package.
The examples perform one tool call, adopt a before-hook block, emit the resulting
observation and release their task. The configured policy determines the decision.

Declare only the installed semantic boundaries, using `version: aw-hook-source/v1`
and `entrypoint: application`. `install(routes)` confirms every requested route
before Ready. `task.emit` accepts input submission, tool-before and tool-after
payloads and returns a decision plus coverage. Applications adopt `block` before
native execution, preserve native permission checks and close tasks after work
has drained. Exact fields and supported effects are in the
[semantic ingress contract](../../../../../src/aw/docs/design/en/runtime/native-sdk.md#shared-semantic-ingress).

A native Hook or language instrumentation source can feed these same callbacks.
Automatic Source discovery and coverage selection remain planned. Adding an
application with these semantics needs only the core SDK and no framework-specific
Rust Adapter. For richer native effects, install the optional framework adapter
packages below and see the [Python/QwenPaw guide](python-sdk.md).

## Install and connect

Install the core candidate tarball (and only the optional framework adapter
tarball your application uses) in the application directory:

```bash
npm install /absolute/artifacts/anolisa-aw-sdk-0.2.2.tgz
# Optional framework adapters (install only when needed):
npm install /absolute/artifacts/anolisa-aw-sdk-qoder-0.2.2.tgz
npm install /absolute/artifacts/anolisa-aw-sdk-pi-0.2.2.tgz
```

Use an absolute socket path in a private directory owned by the application
user. Run AW under that same user with the complete configuration containing
`agents.qoder.adapter: qoder-sdk`. For `native_fallback`, set `required: false`
on every enabled event applicable to this target; explicit Provider denials still apply. Start the service
independently with `aw service serve --config /absolute/aw.yaml` and select the
socket reported by that service. The SDK never starts a service or an Agent.

The installed `@anolisa/aw-sdk-qoder` package includes `examples/qoder.mjs` using
the configuration-driven bridge.
For explicit lifecycle control, Keep the native application's authentication, daemon, model,
permissions, setting sources and Hooks. Pass its existing options to
`qoderOptions(task, options)`, then create the native Query and immediately
`bind(query)` on the returned integration. Every Query needs its own task; close
the Query before closing the task. Close the Instance when the application exits.
Do not combine this integration with the existing AW Query factory on one Query.

Construct `Instance` with an explicit endpoint, target and availability.
`@anolisa/aw-sdk-qoder` exports `qoderProfile` for descriptor loading,
`qoderCapabilities(inputEnabled, sessionStartEnabled)` for registration and
`qoderInstallRoutes(routes, inputEnabled, sessionStartEnabled)` to validate the negotiated routes in
`install(routes)`. For a WarmQuery, create an `Admission` for each entry, pass it
and the native options to `qoderOptions` before startup, and bind the Query when
that entry is consumed. Use the same configured startup flags when wiring callbacks
and registering capabilities. The `install(routes)` callback runs on registration
and again for each task's policy snapshot. Confirm only routes that the
application wires. `cwd` and `environment` default to the application values.
Pass the native Query directory and copied environment in the task or Admission
options when they differ.
Ready records the application's installation acknowledgement; it does not prove
that each future Query has installed Hooks or adopted returned effects.

`await instance.connect()` performs one bounded connection attempt. It returns
`false` only for native-fallback transport absence or an unfinished registration.
Call `instance.start()` to enable a single background reconnect loop; inspect
`state` and `lastError` for readiness or known errors. `await instance.stop()` stops reconnection;
`close()` also releases this application's Scopes and registration. These calls
do not stop the shared Service or the native daemon.

## Availability and task ownership

| Mode | Service absent or connection lost | Explicit error or denial |
| --- | --- | --- |
| `required` | Reject the operation | Preserve the failure or denial |
| `native_fallback` | Return neutral AW callbacks after bounded detection | Preserve the failure or denial |

`timeoutMs` defaults to 1000 ms for lifecycle RPCs. Callback RPCs allow at least
the admitted event budget plus 1000 ms for transport; Service retains the shared
event deadline across ordered steps. The first transport failure retires the
connection. In `native_fallback`, its active tasks remain in fallback and later
callbacks return immediately. In `required`, those tasks remain failed and new
protected tasks are rejected until recovery. Background reconnection retries
transport unavailability in both modes; explicit protocol/configuration errors
stop it. Reconnection registers and confirms Ready for new tasks only. No callback, tool or uncertain Provider invocation is replayed.
Already adopted input/result changes remain part of the native task.

Pass `AbortSignal` to `instance.task({signal})` or `task.invoke`. User cancellation
remains cancellation. Protocol mismatch, malformed input, configuration rejection,
remote deadline errors and Provider failures remain errors. Qoder before-hook
failures deny the tool; post-hook Provider failures stop the Query and are also
available as `integration.failure`. Transport fallback leaves the Query running.
Check `integration.failure` after consumption to surface policy execution errors.

When the Service remains reachable, failed or cancelled callback RPCs drain
their event before propagating the error.
If native code stops between ordered steps, call
`await task.closeEvent(hook, originalPayload)` for that event. Cleanup uses the
original Scope and an independent bounded request, even when the caller's signal
is aborted. A successful close preserves the Task and prevents late steps from
reopening that event. Completed events retain Service automatic retirement.


## Cooperative attach and detach

An application can publish its installed SDK support with `instance.listen()`.
It may start before AW. Once the configured Service is available, discovery lists
the application; it remains detached until requested. Use
`instance.listen({automatic: true})` for automatic registration plus CLI control.
Choose either `start()` or `listen()` as the background owner; await `stop()`
before starting another listener. Both attach modes
use the same registration and Ready path. Direct `connect()` remains available
for explicit application startup before enabling the listener.

From a terminal under the same application user, select the exact
`application_id` returned by the first command:

```bash
AW_SOCKET=/absolute/private/aw.sock
aw instance list --socket "$AW_SOCKET"
AW_SDK_INSTANCE=application_id_from_list
aw connect --socket "$AW_SOCKET" --sdk-instance "$AW_SDK_INSTANCE"
aw disconnect --socket "$AW_SOCKET" --sdk-instance "$AW_SDK_INSTANCE"
```

The result reaches `completed` only after the application confirms Ready or
releases its idle registration. Running tasks reject detach with
`sdk_control_busy`; finish them and retry. The selected instance stays detached
through Service recovery until another attach. Existing fallback tasks stay
native; only new tasks use a recovered registration. Detach leaves the native
application and Service running. `await instance.stop()` withdraws discovery and stops polling;
`close()` additionally releases owned tasks and registration.

Two instances in one PID have distinct IDs. Service restart changes discovery
IDs; list again before selecting. An application without an active SDK listener
cannot be attached. The private socket accepts only the Service user and the
application must acknowledge with its authenticated PID/start time. The CLI
cannot alter the application's availability mode or install Hooks remotely.

`--timeout-ms` defaults to 5000 and accepts 100–30000. A timeout or interrupted
CLI leaves confirmation unknown; inspect `instance list` before another action.
The retained request expires at its Service deadline and late Ready cannot
activate that request. List includes the latest request outcome and connected
registration; general `aw service status` also reports SDK readiness and owner.

## Verified coverage

The Qoder integration uses public `UserPromptSubmit`, `PreToolUse`,
`PostToolUse` and `PostToolUseFailure` callbacks. It preserves existing Hook
groups and appends AW callbacks. The Worker may schedule groups concurrently,
so native observers can still see the original result. Verify the combination
if another Hook also returns an output replacement. AW evaluates its callback
snapshot. Later native rewrites fall outside this Hook's coverage.
SDK 1.0.56 / Worker 1.1.78 acceptance covers real Bash execution, native Hook
coexistence, AW loss during a Query, recovery for a new task and exactly-once
side effects, plus AW and native denials with no tool side effect. The same SDK
and Worker versions adopt a replaced Bash result in the WarmQuery tool event and
the next model turn while an existing native Post Hook sees the original output.
The package does not import Qoder at its root or access private binding files. Other framework versions negotiate their declared capabilities;
native acceptance remains version-specific.

The resident `aw run` path retains its broader managed MCP, credential and
pinned Worker integration. This public-Hook SDK does not yet provide those
additional managed seams. Keep native MCP configuration in the application's
options. Skill checks use the canonical Adapter resolver and require its native
query interfaces. Input rewriting and Skill/MCP combinations require their own native acceptance
before business rollout.

See the [SDK protocol](../../../../../src/aw/docs/design/en/runtime/native-sdk.md)
for ownership and wire contracts and the
[Qoder guide](../agents/qoder-sdk.md) for the existing required entrypoint.
