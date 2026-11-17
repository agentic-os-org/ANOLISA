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
| `tool.after` | `tools/post-execute` | Observe the result or replace eligible text with `replace_result` |
| `security.violation` | After finalization, before top-level Session commit | Check the final result; `block` withholds delivery |

For final-result blocking, the Provider must inspect the complete
`security.violation` result and return `block` on a violation;
`on_error: block` also withholds the result on a check failure. `tool.after`
can replace only a successful root call's single-text result. DSH's
`finalizeContent` may change content after that event, so the final check uses
an independent `security.violation` step.

The DSH plugin also registers native `tools.guard`. It synchronously rechecks
the `tool.before` decision before execution and denies a call that another
plugin allowed by bypassing the pre-execute listener. It makes no second
Provider call. The AW configuration field `events.tool.before.guard` is a
separate reference that runtime admission does not yet support; leave it unset.

## Run and verify

From `src/aw`, run:

```bash
target/debug/aw validate --config ./aw.yaml
target/debug/aw run --config ./aw.yaml --agent dsh -- "Review this repository"
target/debug/aw status --config ./aw.yaml
```

`validate` checks the configuration; `run` also performs Provider admission,
plugin registration, and actual Hook calls. Exercise an allowed call,
before-tool denial, post-tool replacement, and final-result denial. A denied
tool must not execute. For a blocked headless top-level final result, the model
and CLI receive only a withheld error result. To stop the on-demand AW service,
run `target/debug/aw stop --config ./aw.yaml`.

## Coverage

With `security.violation` enabled, the Adapter checks the published DSH
0.2.1-rc.5 Agent-loop build and stops startup on a mismatch. AW does not gate
ordinary entrypoints by version or profile name. Web, SDK, ACP, and custom
profiles have plugin-readiness checks; verify their actual input, tool, and
final-delivery behavior through [native acceptance](../validation.md).

The final check covers the verified headless top-level model, Session, and CLI
delivery. Nested/PTC results and direct `ToolRuntime.execute` consumers do not
cross this commit point; same-process `tools/result` observers see raw content
before the final check. DSH's `skill` tool exposes only its name to the Hook,
so `check_skill` cannot receive a resolved Skill object and fails admission.
