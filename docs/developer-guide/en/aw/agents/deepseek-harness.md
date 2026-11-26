# DeepSeek Harness

[中文版](../../../zh/aw/agents/deepseek-harness.md)

DeepSeek Harness uses one AW configuration through `aw run`. The Adapter
converts DSH input and tool callbacks into AW events; a Provider decides
whether to observe, block, or replace a result. DSH retains its model,
credentials, tool permissions, and profile configuration. Actual headless calls
have been verified on Linux with DSH 0.2.1-rc.5 and Node 24.15.0.

## Prepare and configure

Build AW using the [integration guide](../integration.md), then install and
initialize DSH with its model and credentials in `DSH_HOME`. Prepare a Provider
that implements [`aw-provider/v1alpha1`](../providers/protocol.md). The
repository's [complete configuration](../../../../../src/aw/crates/aw-service/examples/aw.deepseek.yaml)
uses sec-core as one example. To use it directly, replace the Provider, CLI,
and socket paths and start the backend as described in the
[sec-core guide](../providers/sec-core.md). Another Provider can supply the same events.

From the repository's `src/aw` directory, copy the example:

```bash
cp crates/aw-service/examples/aw.deepseek.yaml ./aw.yaml
```

Set `spec.agents.dsh.argv` to the actual DSH executable and the business
profile. The example uses `headless`; task text after `aw run` is passed as
that profile's application argument.

To use another Provider, replace `providers` and `events` under `spec` in the
same `aw.yaml`. The operation names below show where to configure them; use
names declared by that Provider's `describe` response and fill `config` with
its settings.

```yaml
providers:
  policy:
    protocol: aw-provider/v1alpha1
    transport:
      type: stdio
      location: agent
      argv: [/absolute/path/to/provider]
    timeout_ms: 4500
    max_output_bytes: 4096
    config: {}
events:
  input.submit:
    enabled: true
    required: true
    steps:
      - id: check-input
        provider: policy
        operation: check_input
        effects: [observe, block]
        on_error: block
  tool.before:
    enabled: true
    required: true
    steps:
      - id: check-tool
        provider: policy
        operation: check_tool
        effects: [observe, block]
        on_error: block
  tool.after:
    enabled: true
    required: true
    steps:
      - id: transform-result
        provider: policy
        operation: transform_result
        effects: [observe, replace_result]
        on_error: report
  security.violation:
    enabled: true
    required: true
    steps:
      - id: check-final
        provider: policy
        operation: check_final
        effects: [observe, block]
        on_error: block
```

At startup AW calls the Provider's `describe` and `validate_config` methods
and checks its declared operations, events, and effects. Each runtime step
receives a normalized event and returns candidate effects, which the Adapter
adopts in DSH:

| AW event | DSH boundary | Provider effect |
| --- | --- | --- |
| `input.submit` | `agent/pre-step` | Check submitted user text; `block` rejects that submission |
| `tool.before` | `tools/pre-execute` | Check the tool name and arguments; `block` prevents execution |
| `tool.after` | Post-execute or verified rc.5 publication boundary | Observe the result, replace eligible text, or withhold it on a strict check failure |
| `security.violation` | After finalization, before top-level Session commit | Check the final result; `block` withholds delivery |

For final-result blocking, the Provider must inspect the complete
`security.violation` result and return `block` on a violation;
`on_error: block` also withholds the result on a check failure. `tool.after`
can replace only a successful root call's single-text result. Managed rc.5
checks native bash replacements and strict routes after `finalizeContent`.
Other managed replacements run during `tools/post-execute`, before the finalizer.

The DSH plugin also registers native `tools.guard`. It synchronously rechecks
the `tool.before` decision before execution and denies a call that another
plugin allowed by bypassing the pre-execute listener. It makes no second
Provider call. The AW configuration field `events.tool.before.guard` is a
separate reference that runtime admission does not yet support; leave it unset.

## Run and verify

From `src/aw`, run:

```bash
target/debug/aw config validate --config ./aw.yaml
target/debug/aw run --config ./aw.yaml --agent dsh -- "Review this repository"
target/debug/aw service status --config ./aw.yaml
```

`aw config validate` checks the configuration; `run` also performs Provider admission,
plugin registration, and actual Hook calls. Exercise an allowed call,
before-tool denial, post-tool replacement, and final-result denial. A denied
tool must not execute. For a blocked headless top-level final result, the model
and CLI receive only a withheld error result. To stop the on-demand AW service,
run `target/debug/aw service stop --config ./aw.yaml`.

## Application-owned Cordis Bridge

Build AW, the sec-core Provider, the core npm SDK and the optional
`@anolisa/aw-sdk-deepseek-harness` package from the same candidate. Use the
Bridge process's normal workspace and credentials. Set `AW_CONFIG` and
`AW_DESCRIPTOR` to absolute paths in a directory owned by the Bridge user.
Compile the descriptor during deployment and preload the published rc.5 tools
module before any DSH imports. Keep that module external when bundling the Bridge.

```bash
umask 077
aw config descriptor --config "$AW_CONFIG" --agent dsh \
  --profile deepseek-cordis-hooks > "$AW_DESCRIPTOR"
node --import @anolisa/aw-sdk-deepseek-harness/preload bridge.mjs
```

Add the plugin to the existing profile preparation callback and inject
`awDeepseek` into the agent-loop entry. Installation starts the SDK control
listener asynchronously. The Bridge can report application readiness before AW attaches.

```js
import { createDeepSeekBridge } from "@anolisa/aw-sdk-deepseek-harness";

const aw = await createDeepSeekBridge({
  configFile: process.env.AW_CONFIG,
  descriptorFile: process.env.AW_DESCRIPTOR,
});
const context = await bootProfile(ctx => ctx.plugin(aw.plugin));
```

Use the Node Bridge PID in the controller.

```bash
aw connect --config "$AW_CONFIG" --sdk-pid "$BRIDGE_PID" --timeout-ms 5000
```

Wrap the full asynchronous run in
`aw.runTask(sessionId, work, { cwd, signal })`. Start the protected input
inside `work` and await tool completion and session settlement before returning.
In a multi-turn bridge this includes `followup()`, `whenIdle()` and draining pending
children. The SDK admits one run per session and waits up to `attachTimeoutMs`
(default 5000 ms) for AW. Concurrent sessions have independent policy snapshots.
Calls from sessions without an active Task are denied. Child sessions with
distinct IDs need their own ownership binding. Cordis disposal closes the listener.

The cooperative profile covers text `input.submit`, `tool.before` and
`tool.after`. An application's own sanitizer can keep its ordinary `tools/post-execute`
listener. DSH first adopts that listener's content or value and runs its
content finalizer. AW checks the materialized result before `tools/result`,
Session history and model delivery. This ordering also covers matched native
bash results on the rc.5 managed replacement path.

For a successful, non-nested foreground `bash` result, AW authorizes
`content[0].text` and `value.stdout.text` together when their text matches.
sec-core scans the entire result, including stderr, metadata and additional
contexts. Both fields receive the verified masked text. Configure
`tool.after` with `on_error: withhold_result` to deliver a fixed error if a
scan fails, a sensitive result has divergent fields, or strict verification
cannot finish. The error contains no original value, metadata or contexts.
With `on_error: report`, a Provider fault can deliver the original result.
The tool may already have performed its external action.

## Coverage

With `security.violation` enabled, the Adapter checks the published DSH
0.2.1-rc.5 Agent-loop build and stops startup on a mismatch. AW does not gate
ordinary entrypoints by version or profile name. Web, SDK, ACP, and custom
profiles have plugin-readiness checks; verify their actual input, tool, and
final-delivery behavior through [native acceptance](../validation.md).

The final check covers the verified headless top-level model, Session, and CLI
delivery. Nested/PTC results and direct `ToolRuntime.execute` consumers do not
cross this commit point; same-process `tools/result` observers see the tool.after result
before this independent security check. DSH's `skill` tool exposes only its name to the Hook,
so `check_skill` cannot receive a resolved Skill object and fails admission.
